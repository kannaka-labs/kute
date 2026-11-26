//! `kute serve` over TLS with a certificate on both sides, and the
//! commands that call it. Real `kute` processes, real sockets,
//! certificates made for the run and trusted by nothing else.

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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

struct Authority {
    certificate: rcgen::Certificate,
    key: KeyPair,
}

/// Writes `<file>.pem`.
fn authority(dir: &Path, file: &str) -> Authority {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, file);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    std::fs::write(dir.join(format!("{file}.pem")), certificate.pem()).unwrap();
    Authority { certificate, key }
}

impl Authority {
    /// Writes `<file>.pem` and `<file>.key`, for `name` (a host name or
    /// an address).
    fn issue(&self, dir: &Path, file: &str, name: &str) {
        let mut params = CertificateParams::new(vec![name.to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, file);
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.certificate, &self.key).unwrap();
        std::fs::write(dir.join(format!("{file}.pem")), certificate.pem()).unwrap();
        std::fs::write(dir.join(format!("{file}.key")), key.serialize_pem()).unwrap();
    }
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
            if out.read_line(&mut line).unwrap() == 0 {
                let mut why = String::new();
                let _ = child.stderr.take().unwrap().read_to_string(&mut why);
                panic!("the service ended before it said where it listens: {text} {why}");
            }
            text.push_str(&line);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                break value;
            }
        };
        Self { child, address: said["listening"].as_str().unwrap().to_string(), said }
    }

    fn https(&self) -> String {
        format!("https://{}", self.address)
    }
}

/// A service start that is refused, and why.
fn refused(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(["serve", "--listen", "127.0.0.1:0"]).args(args).output().unwrap();
    assert!(!out.status.success(), "kute serve {args:?} should not have started");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const SERVER: [&str; 6] = ["--tls-cert", "server.pem", "--tls-key", "server.key", "--tls-client-ca", "ours.pem"];
const CALLER: [&str; 6] = ["--tls-cert", "caller.pem", "--tls-key", "caller.key", "--tls-ca", "ours.pem"];

/// Certificates and one vocabulary. Returns the vocabulary's digest.
///
/// `ours` issues the service (for 127.0.0.1), `caller` and `second`;
/// `theirs` issues `outsider` and a service of its own, `false-server`.
fn prepare(dir: &Path) -> String {
    let ours = authority(dir, "ours");
    ours.issue(dir, "server", "127.0.0.1");
    ours.issue(dir, "caller", "caller.example");
    ours.issue(dir, "second", "second.example");
    let theirs = authority(dir, "theirs");
    theirs.issue(dir, "outsider", "outsider.example");
    theirs.issue(dir, "false-server", "127.0.0.1");
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    ok(dir, &["train", "--input", "log.jsonl", "--id", "test:tls", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "8", "--output", "held.kvf"])["summary"]["digest"].as_str().unwrap().to_string()
}

fn with<'a>(first: &[&'a str], more: &[&'a str]) -> Vec<&'a str> {
    first.iter().chain(more).copied().collect()
}

#[test]
fn a_service_over_tls_answers_a_caller_with_a_certificate_and_nobody_else() {
    let dir = scratch("http-tls");
    let digest = prepare(&dir);
    let service = Service::start(&dir, &with(&["--vocab", "held.kvf", "--registry"], &SERVER));
    assert_eq!((service.said["tls"].clone(), service.said["registry"].clone()), (json!(true), json!(service.https())), "{}", service.said);
    let at = service.https();

    // With a certificate from the authority the service was given: listed, fetched, the same file.
    let listing = ok(&dir, &with(&["fetch", "--from", &at, "--list"], &CALLER));
    assert_eq!(listing["says_it_holds"].as_array().unwrap().len(), 1, "{listing}");
    ok(&dir, &with(&["fetch", "--from", &at, "--digest", &digest, "--output", "got.kvf"], &CALLER));
    assert_eq!(std::fs::read(dir.join("got.kvf")).unwrap(), std::fs::read(dir.join("held.kvf")).unwrap());

    let nothing = |args: &[&str], file: &str| {
        let said = fails(&dir, args);
        assert!(!dir.join(file).exists(), "{file} was written: {said}");
        said
    };
    // No certificate: refused on this side, before a connection.
    assert!(nothing(&["fetch", "--from", &at, "--digest", &digest, "--output", "a.kvf"], "a.kvf").contains("--tls"));
    // A certificate from an authority the service was not given. The service
    // says why in an alert, and the caller is told (found in review: it was "invalid data").
    let said = nothing(&["fetch", "--from", &at, "--digest", &digest, "--output", "b.kvf", "--tls-cert", "outsider.pem", "--tls-key", "outsider.key", "--tls-ca", "ours.pem"], "b.kvf");
    // The alert can be lost to a reset (seen in CI on Windows); the caller is then told that much.
    assert!(said.contains("this side's that was not accepted") || said.contains("does not accept this side's certificate"), "{said}");
    // A caller that believes another authority does not believe this service.
    assert!(nothing(&["fetch", "--from", &at, "--digest", &digest, "--output", "c.kvf", "--tls-cert", "caller.pem", "--tls-key", "caller.key", "--tls-ca", "theirs.pem"], "c.kvf").contains("tls"));
    // The certificate is for 127.0.0.1, and not for any other name of the same machine.
    let by_name = format!("https://localhost:{}", service.address.rsplit(':').next().unwrap());
    assert!(nothing(&with(&["fetch", "--from", &by_name, "--digest", &digest, "--output", "d.kvf"], &CALLER), "d.kvf").contains("tls"));
    // Plain HTTP to it gets no HTTP back: not a status line, not a word.
    assert!(nothing(&["fetch", "--from", &format!("http://{}", service.address), "--digest", &digest, "--output", "e.kvf"], "e.kvf").contains("registry"));
    let mut plain = TcpStream::connect(&service.address).unwrap();
    plain.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    plain.write_all(b"GET /v1/capabilities HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut answer = Vec::new();
    let _ = plain.read_to_end(&mut answer);
    assert!(!answer.starts_with(b"HTTP/"), "{:?}", String::from_utf8_lossy(&answer));

    // A service with a certificate from elsewhere is not believed, whatever it holds.
    let pretender = Service::start(&dir, &["--vocab", "held.kvf", "--registry", "--tls-cert", "false-server.pem", "--tls-key", "false-server.key", "--tls-client-ca", "ours.pem"]);
    assert!(nothing(&with(&["fetch", "--from", &pretender.https(), "--digest", &digest, "--output", "f.kvf"], &CALLER), "f.kvf").contains("tls"));
}

#[test]
fn only_listed_certificates_are_answered_when_any_are_listed() {
    let dir = scratch("http-tls-allow");
    let digest = prepare(&dir);
    let listed = ok(&dir, &["peer", "fingerprint", "--cert", "caller.pem"])["fingerprint"].as_str().unwrap().to_string();
    let service = Service::start(&dir, &with(&["--vocab", "held.kvf", "--registry", "--allow", &listed], &SERVER));
    assert_eq!(service.said["callers_answered"], json!([listed]), "{}", service.said);
    let at = service.https();
    ok(&dir, &with(&["fetch", "--from", &at, "--digest", &digest, "--output", "got.kvf"], &CALLER));
    // The same authority issued it, and it is not on the list.
    let said = fails(&dir, &["fetch", "--from", &at, "--digest", &digest, "--output", "no.kvf", "--tls-cert", "second.pem", "--tls-key", "second.key", "--tls-ca", "ours.pem"]);
    assert!(said.contains("403") && !dir.join("no.kvf").exists(), "{said}");

    // What cannot be started.
    assert!(refused(&dir, &with(&["--vocab", "held.kvf", "--allow", "sha256:abc"], &SERVER)).contains("not a certificate fingerprint"));
    assert!(refused(&dir, &with(&["--vocab", "held.kvf", "--studio"], &SERVER)).contains("Studio"));
    assert!(refused(&dir, &["--vocab", "held.kvf", "--tls-cert", "server.pem", "--tls-key", "server.key"]).contains("neither was given"));
    assert!(refused(&dir, &["--vocab", "held.kvf", "--tls-cert", "server.pem", "--tls-key", "caller.key", "--tls-client-ca", "ours.pem"]).contains("tls"), "a key that is not the certificate's");
    refused(&dir, &with(&["--vocab", "held.kvf", "--insecure-remote"], &SERVER));
    refused(&dir, &["--vocab", "held.kvf", "--tls-cert", "server.pem"]);
}

#[test]
fn a_connection_that_will_not_finish_the_handshake_costs_a_worker_only_so_long() {
    let dir = scratch("http-tls-stall");
    let digest = prepare(&dir);
    // The handshake has its own time: a request may take 30 s here, a handshake 2.
    let service = Service::start(&dir, &with(&["--vocab", "held.kvf", "--registry", "--workers", "2", "--timeout", "30", "--tls-handshake-timeout", "2"], &SERVER));
    // Every worker is given a connection that says nothing, and two more
    // that start a handshake and then send a byte now and then.
    let silent: Vec<TcpStream> = (0..2).map(|_| TcpStream::connect(&service.address).unwrap()).collect();
    let mut dripping: Vec<TcpStream> = (0..2).map(|_| TcpStream::connect(&service.address).unwrap()).collect();
    for stream in &mut dripping {
        // The start of a handshake record that says 16 KiB follow.
        stream.write_all(&[0x16, 0x03, 0x01, 0x40, 0x00, 0x01]).unwrap();
    }
    let started = Instant::now();
    let drip = std::thread::spawn(move || {
        // A byte every 300 ms for as long as the service takes them.
        let mut gone_after = None;
        while started.elapsed() < Duration::from_secs(15) && gone_after.is_none() {
            std::thread::sleep(Duration::from_millis(300));
            for stream in &mut dripping {
                if stream.write_all(&[0]).is_err() {
                    gone_after = Some(started.elapsed());
                }
            }
        }
        gone_after
    });
    // A real caller is answered once the time a handshake may take has passed.
    ok(&dir, &with(&["fetch", "--from", &service.https(), "--digest", &digest, "--output", "got.kvf", "--timeout", "30"], &CALLER));
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(12), "a real caller waited {waited:?}");
    let gone_after = drip.join().unwrap();
    assert!(gone_after.is_some_and(|after| after < Duration::from_secs(12)), "a dripping connection was kept: {gone_after:?}");
    drop(silent);
}

#[test]
fn a_job_is_given_and_an_upstream_is_asked_over_tls() {
    let dir = scratch("http-tls-job");
    let digest = prepare(&dir);
    std::fs::write(dir.join("text.txt"), "the quick brown fox jumps over the lazy dog and the cat sat on the mat\n".repeat(30)).unwrap();
    let by_hand = ok(&dir, &["train", "--input", "text.txt", "--id", "test:tls:job", "--vocab-size", "300", "--output", "by-hand.kvf"]);
    let service = Service::start(&dir, &with(&["--vocab", "held.kvf", "--registry", "--jobs-dir", "made"], &SERVER));
    let report = ok(&dir, &with(&["job", "--at", &service.https(), "--input", "text.txt", "--id", "test:tls:job", "--vocab-size", "300", "--output", "from-job.kvf"], &CALLER));
    assert_eq!(report["digest"], by_hand["summary"]["digest"], "{report}");
    // Without a certificate, nothing is asked and nothing is trained.
    assert!(fails(&dir, &["job", "--at", &service.https(), "--input", "text.txt", "--id", "test:tls:job", "--vocab-size", "300", "--output", "no.kvf"]).contains("--tls"));
    assert!(!dir.join("no.kvf").exists());

    // A plain registry on this machine that asks the TLS one for what it does not hold.
    let front = Service::start(&dir, &["--registry-only", "--upstream", &service.https(), "--tls-cert", "caller.pem", "--tls-key", "caller.key", "--tls-ca", "ours.pem"]);
    assert_eq!(front.said["tls"], json!(false), "{}", front.said);
    ok(&dir, &["fetch", "--from", &format!("http://{}", front.address), "--digest", &digest, "--output", "through.kvf"]);
    assert_eq!(std::fs::read(dir.join("through.kvf")).unwrap(), std::fs::read(dir.join("held.kvf")).unwrap());
    // An https upstream and no certificate to show it stops the start.
    assert!(refused(&dir, &["--registry-only", "--upstream", &service.https()]).contains("--tls"));

    // Over TLS, an address that is not loopback is served with no --insecure-remote.
    // Jobs there need the callers named: an authority's whole issue may read, not train.
    let unnamed = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(["serve", "--listen", "0.0.0.0:0", "--vocab", "held.kvf", "--jobs-dir", "made-open"]).args(SERVER).output().unwrap();
    assert!(!unnamed.status.success() && String::from_utf8_lossy(&unnamed.stderr).contains("needs --allow"), "{}", String::from_utf8_lossy(&unnamed.stderr));
    assert!(!dir.join("made-open").exists(), "the directory was made for a service that did not start");
    let caller = ok(&dir, &["peer", "fingerprint", "--cert", "caller.pem"])["fingerprint"].as_str().unwrap().to_string();
    let mut open = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(["serve", "--listen", "0.0.0.0:0", "--vocab", "held.kvf", "--jobs-dir", "made-open", "--allow", &caller]).args(SERVER).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut line = String::new();
    let mut text = String::new();
    let mut out = BufReader::new(open.stdout.take().unwrap());
    let said: Value = loop {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "it did not start: {text}");
        text.push_str(&line);
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            break value;
        }
    };
    let _ = open.kill();
    let _ = open.wait();
    assert_eq!((said["tls"].clone(), said["jobs"].clone(), said["loopback_only"].clone()), (json!(true), json!(true), json!(false)), "{said}");
}
