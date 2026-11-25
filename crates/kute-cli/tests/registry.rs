//! `kute serve --registry` and `kute fetch`: vocabulary files given by
//! digest, read-only, and a client that keeps a file only if it is the
//! one it asked for. The registry side is a real `kute serve` process;
//! the dishonest registries are sockets in this file.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

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
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(["serve", "--listen", "127.0.0.1:0"]).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
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

    fn raw(&self, request: &str) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let at = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(answer[..at].to_vec()).unwrap();
        (head.split(' ').nth(1).unwrap().parse().unwrap(), head, answer[at + 4..].to_vec())
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let (status, _, body) = self.raw(&format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"));
        (status, serde_json::from_slice(&body).unwrap())
    }
}

/// A registry that answers every request with these bytes, whatever was
/// asked, `times` times. Returns its address.
fn impostor(answer: Vec<u8>, times: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for _ in 0..times {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut seen = Vec::new();
            let mut chunk = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = stream.write_all(&answer);
        }
    });
    format!("http://{address}")
}

fn with_length(body: &[u8]) -> Vec<u8> {
    let mut answer = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    answer.extend_from_slice(body);
    answer
}

/// Two vocabularies, one signed by `ours.pem`. Returns (digest of the
/// signed one, digest of the other, our public key, another public key).
fn prepare(dir: &Path) -> (String, String, String, String) {
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    for (id, size, file) in [("test:registry:signed", "8", "plain.kvf"), ("test:registry:other", "7", "other.kvf")] {
        ok(dir, &["train", "--input", "log.jsonl", "--id", id, "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", size, "--output", file]);
    }
    let ours = ok(dir, &["keygen", "--output", "ours.pem"])["public_key"].as_str().unwrap().to_string();
    let theirs = ok(dir, &["keygen", "--output", "theirs.pem"])["public_key"].as_str().unwrap().to_string();
    let signed = ok(dir, &["sign", "--vocab", "plain.kvf", "--key", "ours.pem", "--output", "signed.kvf"])["digest"].as_str().unwrap().to_string();
    let other = ok(dir, &["inspect", "other.kvf"])["summary"]["digest"].as_str().unwrap().to_string();
    (signed, other, ours, theirs)
}

#[test]
fn a_vocabulary_is_fetched_by_digest_and_kept_only_if_it_is_that_one() {
    let dir = scratch("registry");
    let (signed, other, ours, theirs) = prepare(&dir);
    let registry = Service::start(&dir, &["--vocab", "signed.kvf", "--vocab", "other.kvf", "--registry"]);
    let url = registry.url();
    assert_eq!(registry.said["registry"], json!(url));

    // The listing is the registry's account: both files, and who it says signed them.
    let listing = ok(&dir, &["fetch", "--from", &url, "--list"]);
    let rows = listing["says_it_holds"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listing}");
    let row = rows.iter().find(|r| r["digest"] == json!(signed)).unwrap();
    assert_eq!((row["id"].clone(), row["signed_by"].as_array().unwrap().len()), (json!("test:registry:signed"), 1), "{row}");
    assert!(listing["checked"].as_str().unwrap().starts_with("nothing"));

    // Fetched with a trusted key: written, the same digest, and it verifies.
    let got = ok(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "got.kvf", "--trusted-key", &ours]);
    assert_eq!((got["digest"].clone(), got["id"].clone()), (json!(signed), json!("test:registry:signed")), "{got}");
    assert_eq!(ok(&dir, &["verify", "--vocab", "got.kvf", "--trusted-key", &ours])["verified"], true);
    assert_eq!(ok(&dir, &["inspect", "got.kvf"])["summary"]["digest"], json!(signed));
    // Without a key the digest alone is checked, and the report says so.
    let unsigned = ok(&dir, &["fetch", "--from", &url, "--digest", &other, "--output", "got-other.kvf"]);
    assert!(unsigned["signature"].as_str().unwrap().starts_with("not checked"), "{unsigned}");
    assert_eq!(unsigned["signatures_in_the_file"], 0);
    // A signed file fetched without a key keeps its signature, and the report says nobody checked it.
    let unchecked = ok(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "got-unchecked.kvf"]);
    assert_eq!(unchecked["signatures_in_the_file"], 1);
    assert!(unchecked["signatures_in_the_file_means"].as_str().unwrap().starts_with("none was checked"), "{unchecked}");
    assert_eq!(got["signatures_in_the_file"], 1);

    // Not kept: signed by nobody trusted, signed by somebody else, not held, already there.
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &other, "--output", "no1.kvf", "--trusted-key", &ours]).contains("carries no signature"));
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "no2.kvf", "--trusted-key", &theirs]).contains("none of its signatures"));
    let absent = format!("sha256:{}", "0".repeat(64));
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &absent, "--output", "no3.kvf"]).contains("404"));
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "got.kvf"]).contains("--force"));
    ok(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "got.kvf", "--force"]);
    // Refused before anything is asked for.
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", "latest", "--output", "no4.kvf"]).contains("not a digest"));
    assert!(fails(&dir, &["fetch", "--from", "https://example.invalid", "--digest", &signed, "--output", "no5.kvf"]).contains("https is not built"));
    // A name that is not there is an error, and one within the time given.
    let started = std::time::Instant::now();
    fails(&dir, &["fetch", "--from", "http://registry.invalid:8787", "--digest", &signed, "--output", "no5.kvf", "--timeout", "5"]);
    assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "no6.kvf", "--trusted-key", "not-a-key"]).contains("trusted key"));
    assert!(fails(&dir, &["fetch", "--from", &url, "--digest", &signed, "--output", "no7.kvf", "--max-bytes", "100"]).contains("--max-bytes"));
    for name in ["no1.kvf", "no2.kvf", "no3.kvf", "no4.kvf", "no5.kvf", "no6.kvf", "no7.kvf"] {
        assert!(!dir.join(name).exists(), "{name} was written");
    }

    // The registry is read-only, and a file is named by a digest and nothing else.
    let (status, head, _) = registry.raw("POST /v1/vocabularies HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
    assert!(status == 405 && head.contains("Allow: GET"), "{head}");
    assert_eq!(registry.raw(&format!("PUT /v1/vocabularies/{signed} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")).0, 405);
    for path in ["/v1/vocabularies/test:registry:signed", "/v1/vocabularies/", "/v1/vocabularies/../../etc/passwd", &format!("/v1/vocabularies/{}", signed.to_uppercase()), &format!("/v1/vocabularies/{signed}/x")] {
        assert_eq!(registry.get(path).0, 404, "{path}");
    }
    // On loopback it answers loopback names only, like the rest of the service.
    assert_eq!(registry.raw("GET /v1/vocabularies HTTP/1.1\r\nHost: evil.example\r\n\r\n").0, 403);
    // The operations are still there beside it.
    let capabilities = registry.get("/v1/capabilities").1;
    assert_eq!((capabilities["registry"].clone(), capabilities["operations"].as_array().unwrap().len()), (json!(true), 5));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_registry_exists_only_when_asked_for_and_can_be_all_there_is() {
    let dir = scratch("registry-modes");
    let (signed, _, _, _) = prepare(&dir);

    // Without the option the paths are not there, and nothing says what is held.
    let plain = Service::start(&dir, &["--vocab", "signed.kvf"]);
    assert_eq!(plain.said["registry"], Value::Null);
    assert_eq!(plain.get("/v1/vocabularies").0, 404);
    assert_eq!(plain.get(&format!("/v1/vocabularies/{signed}")).0, 404);
    assert_eq!(plain.get("/v1/capabilities").1["registry"], false);
    assert!(fails(&dir, &["fetch", "--from", &plain.url(), "--digest", &signed, "--output", "no.kvf"]).contains("does not give vocabulary files"));
    assert!(!dir.join("no.kvf").exists());

    // As a registry and nothing else: files are given, nothing is computed.
    let only = Service::start(&dir, &["--vocab", "signed.kvf", "--registry-only"]);
    assert_eq!((only.said["registry"].clone(), only.said["operations"].clone()), (json!(only.url()), json!(false)));
    ok(&dir, &["fetch", "--from", &only.url(), "--digest", &signed, "--output", "from-only.kvf"]);
    let body = json!({ "vocabulary": "test:registry:signed", "text": "{\"event\":\"OPEN\"}\n" }).to_string();
    for operation in ["encode", "inspect", "discover"] {
        let (status, _, _) = only.raw(&format!("POST /v1/{operation} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert_eq!(status, 404, "{operation}");
    }
    assert_eq!(only.get("/v1/capabilities").1["operations"], json!([]));
    // Studio is a page over the operations; without them there is nothing for it to show.
    assert!(!kute(&dir, &["serve", "--listen", "127.0.0.1:0", "--registry-only", "--studio"]).status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_registry_that_lies_or_stalls_gets_nothing_written() {
    let dir = scratch("registry-liar");
    let (signed, _, ours, _) = prepare(&dir);
    let other = std::fs::read(dir.join("other.kvf")).unwrap();
    let wanted = std::fs::read(dir.join("signed.kvf")).unwrap();

    // Another vocabulary than the one asked for, whole and valid in itself.
    let said = fails(&dir, &["fetch", "--from", &impostor(with_length(&other), 1), "--digest", &signed, "--output", "a.kvf"]);
    assert!(said.contains("not the one asked for"), "{said}");
    // The right content with the signature taken off, to someone who requires one.
    let stripped = std::fs::read(dir.join("plain.kvf")).unwrap();
    let said = fails(&dir, &["fetch", "--from", &impostor(with_length(&stripped), 1), "--digest", &signed, "--output", "b.kvf", "--trusted-key", &ours]);
    assert!(said.contains("carries no signature"), "{said}");
    // One byte of the content changed: it is another vocabulary, or none.
    let mut altered = wanted.clone();
    let at = altered.windows(4).position(|w| w == b"OPEN").unwrap();
    altered[at] = b'X';
    fails(&dir, &["fetch", "--from", &impostor(with_length(&altered), 1), "--digest", &signed, "--output", "c.kvf"]);
    // Not a vocabulary; cut short; no stated length; a length past the limit; not HTTP; a stall.
    fails(&dir, &["fetch", "--from", &impostor(with_length(b"{\"hello\":1}"), 1), "--digest", &signed, "--output", "d.kvf"]);
    let mut short = with_length(&wanted);
    short.truncate(short.len() - 10);
    assert!(fails(&dir, &["fetch", "--from", &impostor(short, 1), "--digest", &signed, "--output", "e.kvf"]).contains("before the whole answer"));
    let mut unstated = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    unstated.extend_from_slice(&wanted);
    assert!(fails(&dir, &["fetch", "--from", &impostor(unstated, 1), "--digest", &signed, "--output", "f.kvf"]).contains("does not state its length"));
    let huge = b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999\r\n\r\n".to_vec();
    assert!(fails(&dir, &["fetch", "--from", &impostor(huge, 1), "--digest", &signed, "--output", "g.kvf"]).contains("--max-bytes"));
    assert!(fails(&dir, &["fetch", "--from", &impostor(b"SSH-2.0-nothing\r\n\r\n".to_vec(), 1), "--digest", &signed, "--output", "h.kvf"]).contains("not HTTP"));
    let started = std::time::Instant::now();
    let stalled = b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\npart".to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let holder = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.write_all(&stalled);
        // Held open, saying nothing more, for longer than the client waits.
        std::thread::sleep(Duration::from_secs(6));
    });
    assert!(fails(&dir, &["fetch", "--from", &format!("http://{address}"), "--digest", &signed, "--output", "i.kvf", "--timeout", "2"]).contains("in time"));
    assert!(started.elapsed() < Duration::from_secs(6), "the deadline did not hold: {:?}", started.elapsed());
    let _ = holder.join();
    for name in ["a.kvf", "b.kvf", "c.kvf", "d.kvf", "e.kvf", "f.kvf", "g.kvf", "h.kvf", "i.kvf"] {
        assert!(!dir.join(name).exists(), "{name} was written");
    }
    // And an honest answer from the same kind of socket is kept, so the refusals above are about content.
    ok(&dir, &["fetch", "--from", &impostor(with_length(&wanted), 1), "--digest", &signed, "--output", "j.kvf", "--trusted-key", &ours]);
    let _ = std::fs::remove_dir_all(&dir);
}
