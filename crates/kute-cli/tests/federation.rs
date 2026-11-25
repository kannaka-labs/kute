//! Registries that ask other registries (`kute serve --registry
//! --upstream`), and a client that asks several (`kute fetch --from A
//! --from B`). Every registry here is a real `kute serve` process; the
//! dishonest, slow and counting upstreams are sockets in this file.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kute(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let out = kute(dir, args);
    assert!(out.status.success(), "kute {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The command fails, and says why.
fn fails(dir: &Path, args: &[&str]) -> String {
    let out = kute(dir, args);
    assert!(!out.status.success(), "kute {args:?} should have failed: {}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).into_owned()
}

struct Service {
    child: Child,
    address: String,
    said: Value,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    fn start(dir: &Path, args: &[&str]) -> Self {
        Self::start_at(dir, "127.0.0.1:0", args)
    }

    fn start_at(dir: &Path, listen: &str, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(["serve", "--listen", listen]).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut text = String::new();
        let said: Value = loop {
            let mut line = String::new();
            assert!(out.read_line(&mut line).unwrap() > 0, "the service ended before it said where it listens: {text}");
            text.push_str(&line);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                break value;
            }
        };
        Self { child, address: said["listening"].as_str().unwrap().to_string(), said }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Status and body of a GET, with extra header lines.
    fn get(&self, path: &str, headers: &str) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n").as_bytes()).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let at = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(answer[..at].to_vec()).unwrap();
        (head.split(' ').nth(1).unwrap().parse().unwrap(), answer[at + 4..].to_vec())
    }
}

/// An upstream that is a socket: it answers every request with what
/// `answer` makes of the request's first line, counts the requests, and
/// keeps the header lines of the last one. Returns its address.
fn socket(answer: impl Fn(&str) -> Vec<u8> + Send + 'static) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let count = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(std::sync::Mutex::new(String::new()));
    let (counted, kept) = (count.clone(), last.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut seen = Vec::new();
            let mut chunk = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&chunk[..n]),
                }
            }
            let head = String::from_utf8_lossy(&seen).into_owned();
            counted.fetch_add(1, Ordering::SeqCst);
            *kept.lock().unwrap() = head.clone();
            let _ = stream.write_all(&answer(head.lines().next().unwrap_or("")));
        }
    });
    (format!("http://{address}"), count, last)
}

fn with_length(status: &str, body: &[u8]) -> Vec<u8> {
    let mut answer = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    answer.extend_from_slice(body);
    answer
}

/// An address nothing listens on.
fn dead() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

/// Two vocabularies, one signed by `ours.pem`. Returns (digest of the
/// signed one, digest of the other, our public key, another public key).
fn prepare(dir: &Path) -> (String, String, String, String) {
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    for (id, size, file) in [("test:federation:signed", "8", "plain.kvf"), ("test:federation:other", "7", "other.kvf")] {
        ok(dir, &["train", "--input", "log.jsonl", "--id", id, "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", size, "--output", file]);
    }
    let ours = ok(dir, &["keygen", "--output", "ours.pem"])["public_key"].as_str().unwrap().to_string();
    let theirs = ok(dir, &["keygen", "--output", "theirs.pem"])["public_key"].as_str().unwrap().to_string();
    let signed = ok(dir, &["sign", "--vocab", "plain.kvf", "--key", "ours.pem", "--output", "signed.kvf"])["digest"].as_str().unwrap().to_string();
    let other = ok(dir, &["inspect", "other.kvf"])["summary"]["digest"].as_str().unwrap().to_string();
    (signed, other, ours, theirs)
}

const NOBODY: &str = "sha256:00000000000000000000000000000000000000000000000000000000000000aa";

#[test]
fn a_registry_asks_its_upstream_and_a_question_goes_one_registry_further_and_no_more() {
    let dir = scratch("federation-chain");
    let (signed, other, ours, theirs) = prepare(&dir);
    // A holds the signed vocabulary. B holds the other and asks A. C holds neither and asks B.
    let a = Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf"]);
    let b = Service::start(&dir, &["--registry-only", "--vocab", "other.kvf", "--upstream", &a.url()]);
    let c = Service::start(&dir, &["--registry-only", "--upstream", &b.url()]);
    assert_eq!(b.said["upstreams"][0], a.url());
    assert_eq!(a.said["upstreams"].as_array().unwrap().len(), 0);

    // What B says it holds is what it holds; who it asks is named beside it.
    let listing = ok(&dir, &["fetch", "--from", &b.url(), "--list"]);
    assert_eq!(listing["says_it_holds"].as_array().unwrap().len(), 1, "{listing}");
    assert_eq!(listing["says_it_holds"][0]["digest"], other);
    assert_eq!(listing["says_it_asks"][0], a.url());
    // Several registries are listed a row each, a dead one being its row.
    let gone = dead();
    let listing = ok(&dir, &["fetch", "--from", &a.url(), "--from", &gone, "--from", &c.url(), "--list", "--timeout", "3"]);
    let rows = listing["registries"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{listing}");
    assert_eq!(rows[0]["says_it_holds"][0]["digest"], signed);
    assert!(rows[1]["error"].is_string(), "{listing}");
    assert_eq!(rows[2]["says_it_holds"].as_array().unwrap().len(), 0);

    // C is asked for what only A holds. C asks B; B, asked by a registry,
    // answers from what it has and asks nobody. Two registries away is too far.
    let said = fails(&dir, &["fetch", "--from", &c.url(), "--digest", &signed, "--output", "far.kvf"]);
    assert!(said.contains("404") && said.contains("nothing was written"), "{said}");
    assert!(!dir.join("far.kvf").exists());
    // The same holds for anyone who sends the header a registry sends.
    assert_eq!(b.get(&format!("/v1/vocabularies/{signed}"), "Kute-Registry-Via: 1\r\n").0, 404);
    // What C holds through B is one registry away, and is given.
    let report = ok(&dir, &["fetch", "--from", &c.url(), "--digest", &other, "--output", "near.kvf"]);
    assert_eq!(report["digest"], other);

    // B is asked directly: it asks A, checks what comes, and gives it on
    // with the signature it came with.
    let report = ok(&dir, &["fetch", "--from", &b.url(), "--digest", &signed, "--output", "through.kvf", "--trusted-key", &ours]);
    assert_eq!(report["registry"], b.url());
    assert_eq!(report["registries_passed_over"].as_array().unwrap().len(), 0);
    assert_eq!(ok(&dir, &["inspect", "through.kvf"])["summary"]["digest"], signed);
    // Someone who trusts another key is still refused: B vouches for nothing.
    let said = fails(&dir, &["fetch", "--from", &b.url(), "--digest", &signed, "--output", "untrusted.kvf", "--trusted-key", &theirs]);
    assert!(said.contains("none of its signatures"), "{said}");
    // B's listing has not grown: it lists what it was started with.
    assert_eq!(ok(&dir, &["fetch", "--from", &b.url(), "--list"])["says_it_holds"].as_array().unwrap().len(), 1);
    // B has kept the file, so now it can answer a registry that asks. C
    // was told no a moment ago and remembers that for a minute; a registry
    // that has not asked yet reaches it.
    assert_eq!(c.get(&format!("/v1/vocabularies/{signed}"), "").0, 404);
    let d = Service::start(&dir, &["--registry-only", "--upstream", &b.url()]);
    ok(&dir, &["fetch", "--from", &d.url(), "--digest", &signed, "--output", "kept-far.kvf", "--trusted-key", &ours]);
    // And B answers with A gone.
    drop(a);
    ok(&dir, &["fetch", "--from", &b.url(), "--digest", &signed, "--output", "kept.kvf", "--trusted-key", &ours]);
    // A digest nobody has is a 404, with the upstream gone too.
    assert_eq!(b.get(&format!("/v1/vocabularies/{NOBODY}"), "").0, 404);
    // What B holds itself never needed anyone.
    assert_eq!(b.get(&format!("/v1/vocabularies/{other}"), "").0, 200);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registries_that_name_each_other_do_not_pass_a_question_round() {
    let dir = scratch("federation-ring");
    let (signed, other, _, _) = prepare(&dir);
    // Two addresses, each named as the other's upstream before either is up.
    let (x_at, y_at) = (dead().replace("http://", ""), dead().replace("http://", ""));
    let x = Service::start_at(&dir, &x_at, &["--registry-only", "--vocab", "signed.kvf", "--upstream", &format!("http://{y_at}"), "--upstream", &format!("http://{x_at}")]);
    let y = Service::start_at(&dir, &y_at, &["--registry-only", "--vocab", "other.kvf", "--upstream", &format!("http://{x_at}")]);
    let started = Instant::now();
    // Nobody has it: X asks Y and itself once each, and each answers from what it has.
    assert_eq!(x.get(&format!("/v1/vocabularies/{NOBODY}"), "").0, 404);
    assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    // Each gives what the other holds.
    ok(&dir, &["fetch", "--from", &x.url(), "--digest", &other, "--output", "from-x.kvf"]);
    ok(&dir, &["fetch", "--from", &y.url(), "--digest", &signed, "--output", "from-y.kvf"]);
    assert_eq!(y.get(&format!("/v1/vocabularies/{NOBODY}"), "").0, 404);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_upstream_that_lies_is_passed_over_and_what_it_sent_is_not_given_on() {
    let dir = scratch("federation-liar");
    let (signed, _, ours, _) = prepare(&dir);
    let other = std::fs::read(dir.join("other.kvf")).unwrap();
    let stripped = std::fs::read(dir.join("plain.kvf")).unwrap();
    let honest = Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf"]);

    // An upstream that answers every question with another vocabulary,
    // whole and valid in itself.
    let (liar, asked, last) = socket(move |_| with_length("200 OK", &other));
    let alone = Service::start(&dir, &["--registry-only", "--upstream", &liar]);
    assert_eq!(alone.get(&format!("/v1/vocabularies/{signed}"), "").0, 404);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    // What the registry sent upstream: the digest, and the mark that it is a registry asking.
    let sent = last.lock().unwrap().to_ascii_lowercase();
    assert!(sent.starts_with(&format!("get /v1/vocabularies/{signed} ")) && sent.contains("kute-registry-via: 1"), "{sent}");
    // A failure is not remembered as absence: the upstream is asked again.
    assert_eq!(alone.get(&format!("/v1/vocabularies/{signed}"), "").0, 404);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // With an honest upstream named after it, the liar costs a question and nothing else.
    let both = Service::start(&dir, &["--registry-only", "--upstream", &liar, "--upstream", &honest.url()]);
    ok(&dir, &["fetch", "--from", &both.url(), "--digest", &signed, "--output", "past-the-liar.kvf", "--trusted-key", &ours]);

    // An upstream that sends the right content with the signature taken
    // off. The digest does not cover signatures, so a registry with no key
    // of its own passes it on, and the client who requires one refuses it.
    let (stripper, _, _) = socket(move |_| with_length("200 OK", &stripped));
    let open = Service::start(&dir, &["--registry-only", "--upstream", &stripper, "--upstream", &honest.url()]);
    let said = fails(&dir, &["fetch", "--from", &open.url(), "--digest", &signed, "--output", "stripped.kvf", "--trusted-key", &ours]);
    assert!(said.contains("carries no signature"), "{said}");
    assert!(!dir.join("stripped.kvf").exists());
    // A registry told which key to require does not keep the stripped
    // file, and goes on to the upstream that has it signed.
    let strict = Service::start(&dir, &["--registry-only", "--upstream", &stripper, "--upstream", &honest.url(), "--upstream-trusted-key", &ours]);
    assert_eq!(strict.said["upstream_files_need_a_trusted_signature"], true);
    ok(&dir, &["fetch", "--from", &strict.url(), "--digest", &signed, "--output", "signed-again.kvf", "--trusted-key", &ours]);
    // And with only the stripper to ask, it has nothing to give.
    let strict_alone = Service::start(&dir, &["--registry-only", "--upstream", &stripper, "--upstream-trusted-key", &ours]);
    assert_eq!(strict_alone.get(&format!("/v1/vocabularies/{signed}"), "").0, 404);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn asking_upstream_is_bounded_one_question_at_a_time_and_absence_is_remembered() {
    let dir = scratch("federation-bounds");
    let (signed, _, _, _) = prepare(&dir);
    let held = signed.clone();

    // An upstream that says no, and counts how often it is asked.
    let (no, asked, _) = socket(|_| with_length("404 Not Found", b"{}"));
    let registry = Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf", "--upstream", &no]);
    for _ in 0..5 {
        assert_eq!(registry.get(&format!("/v1/vocabularies/{NOBODY}"), "").0, 404);
    }
    assert_eq!(asked.load(Ordering::SeqCst), 1, "a digest no upstream had is not asked for again at once");
    // Something that is not a digest is never sent upstream.
    assert_eq!(registry.get("/v1/vocabularies/sha256:zz", "").0, 404);
    assert_eq!(registry.get("/v1/vocabularies/..%2f..%2fetc", "").0, 404);
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    // An upstream that accepts and says nothing. While the registry waits
    // on it for one caller, another caller is told to come back at once,
    // and a file held here is given without waiting.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent = format!("http://{}", listener.local_addr().unwrap());
    let holder = std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().take(1).collect();
        std::thread::sleep(Duration::from_secs(6));
        drop(held);
    });
    let slow = Arc::new(Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf", "--upstream", &silent, "--upstream-timeout", "3"]));
    let first = {
        let slow = slow.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let status = slow.get(&format!("/v1/vocabularies/{NOBODY}"), "").0;
            (status, started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(700));
    let started = Instant::now();
    let second = "sha256:00000000000000000000000000000000000000000000000000000000000000bb";
    let (status, body) = slow.get(&format!("/v1/vocabularies/{second}"), "");
    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    assert_eq!(slow.get(&format!("/v1/vocabularies/{held}"), "").0, 200);
    assert_eq!(slow.get("/v1/vocabularies", "").0, 200);
    assert!(started.elapsed() < Duration::from_secs(2), "callers waited behind the upstream: {:?}", started.elapsed());
    let (status, took) = first.join().unwrap();
    assert_eq!(status, 404);
    assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(6), "the upstream deadline did not hold: {took:?}");
    let _ = holder.join();

    // A file larger than the registry will take from upstream is not taken.
    let honest = Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf"]);
    let small = Service::start(&dir, &["--registry-only", "--upstream", &honest.url(), "--upstream-max-bytes", "100"]);
    assert_eq!(small.get(&format!("/v1/vocabularies/{signed}"), "").0, 404);
    // A cache too small to hold a file still gives it, and asks again next time.
    let (relay, relayed, _) = {
        let file = std::fs::read(dir.join("signed.kvf")).unwrap();
        socket(move |_| with_length("200 OK", &file))
    };
    let forgetful = Service::start(&dir, &["--registry-only", "--upstream", &relay, "--upstream-cache-bytes", "100"]);
    let keeping = Service::start(&dir, &["--registry-only", "--upstream", &relay]);
    for _ in 0..3 {
        assert_eq!(forgetful.get(&format!("/v1/vocabularies/{signed}"), "").0, 200);
    }
    assert_eq!(relayed.load(Ordering::SeqCst), 3);
    for _ in 0..3 {
        assert_eq!(keeping.get(&format!("/v1/vocabularies/{signed}"), "").0, 200);
    }
    assert_eq!(relayed.load(Ordering::SeqCst), 4, "a kept file is not asked for again");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_client_asks_several_registries_in_order_and_says_which_it_passed_over() {
    let dir = scratch("federation-client");
    let (signed, other, ours, _) = prepare(&dir);
    let wrong = std::fs::read(dir.join("other.kvf")).unwrap();
    let (liar, _, _) = socket(move |_| with_length("200 OK", &wrong));
    let gone = dead();
    let empty = Service::start(&dir, &["--registry-only", "--vocab", "other.kvf"]);
    let honest = Service::start(&dir, &["--registry-only", "--vocab", "signed.kvf"]);

    let report = ok(&dir, &["fetch", "--from", &gone, "--from", &liar, "--from", &empty.url(), "--from", &honest.url(), "--from", &dead(), "--digest", &signed, "--output", "got.kvf", "--trusted-key", &ours, "--timeout", "5"]);
    assert_eq!(report["registry"], honest.url());
    let passed = report["registries_passed_over"].as_array().unwrap();
    assert_eq!(passed.len(), 3, "{report}");
    assert_eq!(passed[0]["registry"], gone);
    assert!(passed[1]["why"].as_str().unwrap().contains("not the one asked for"), "{report}");
    assert!(passed[2]["why"].as_str().unwrap().contains("404"), "{report}");
    assert_eq!(ok(&dir, &["inspect", "got.kvf"])["summary"]["digest"], signed);

    // Nobody gives it: nothing is written, and each registry's reason is said.
    let said = fails(&dir, &["fetch", "--from", &gone, "--from", &liar, "--from", &empty.url(), "--digest", &signed, "--output", "none.kvf", "--timeout", "5"]);
    assert!(said.contains("nothing was written") && said.contains(&gone) && said.contains(&liar) && said.contains(&empty.url()), "{said}");
    assert!(!dir.join("none.kvf").exists());
    // An address that cannot be read is refused before anyone is asked.
    let said = fails(&dir, &["fetch", "--from", &honest.url(), "--from", "https://example.invalid", "--digest", &other, "--output", "never.kvf"]);
    assert!(said.contains("https"), "{said}");
    assert!(!dir.join("never.kvf").exists());

    // Starting a registry: an upstream needs a registry, a readable address and, for a key, an upstream.
    for bad in [
        vec!["--vocab", "other.kvf", "--upstream", "http://127.0.0.1:9"],
        vec!["--registry", "--vocab", "other.kvf", "--upstream", "https://example.invalid"],
        vec!["--registry", "--vocab", "other.kvf", "--upstream", "not an address"],
        vec!["--registry", "--vocab", "other.kvf", "--upstream-trusted-key", &ours],
        vec!["--registry", "--vocab", "other.kvf", "--upstream", "http://127.0.0.1:9", "--upstream-trusted-key", "no-such-key.pem"],
    ] {
        let mut args = vec!["serve", "--listen", "127.0.0.1:0"];
        args.extend(bad.iter().copied());
        let out = Command::new("timeout").arg("20").arg(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(&args).output();
        // Where there is no `timeout` command the binary is run directly:
        // a refusal ends at once either way.
        let out = out.unwrap_or_else(|_| kute(&dir, &args));
        assert!(!out.status.success(), "{bad:?} was accepted");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
