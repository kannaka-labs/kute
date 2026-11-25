//! Gate AC-701 of the v0.7 specification, at the level it is written:
//! "Two independent authenticated processes exchange and translate data;
//! unauthorized peers are refused." Two `kute` processes, real sockets,
//! certificates made for the run and trusted by nothing else.

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

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
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

struct Authority {
    certificate: rcgen::Certificate,
    key: KeyPair,
}

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
    /// Writes `<file>.pem` and `<file>.key`.
    fn issue(&self, dir: &Path, file: &str, name: &str) {
        let mut params = CertificateParams::new(vec![name.to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.certificate, &self.key).unwrap();
        std::fs::write(dir.join(format!("{file}.pem")), certificate.pem()).unwrap();
        std::fs::write(dir.join(format!("{file}.key")), key.serialize_pem()).unwrap();
    }
}

fn free_address() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    format!("127.0.0.1:{port}")
}

fn serve(dir: &Path, address: &str, extra: &[&str]) -> Child {
    let mut args = vec!["peer", "serve", "--listen", address, "--accept-vocabularies", "--output-dir", "received"];
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
}

/// Run `kute peer send` until the listener is up. A refusal by the peer is
/// also an answer, so stop on anything other than a failure to connect.
fn send(dir: &Path, address: &str, extra: &[&str]) -> Output {
    let mut args = vec!["peer", "send", "--connect", address, "--vocab", "events.kvf", "--input", "message.jsonl"];
    args.extend_from_slice(extra);
    let mut last = None;
    for _ in 0..100 {
        let out = kute(dir, &args);
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        let not_up_yet = !out.status.success() && (stderr.contains("refused") && stderr.contains("os error") || stderr.contains("actively refused"));
        last = Some(out);
        if !not_up_yet {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last.unwrap()
}

#[test]
fn two_authenticated_processes_exchange_and_an_unauthorized_one_is_refused() {
    let dir = scratch("peer-tls");
    let cycle = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];
    let line = |n: &str| format!("{{\"event\":\"{n}\"}}\n");
    std::fs::write(dir.join("log.jsonl"), cycle.iter().map(|n| line(n)).collect::<String>().repeat(40)).unwrap();
    std::fs::write(dir.join("message.jsonl"), cycle.iter().map(|n| line(n)).collect::<String>()).unwrap();
    ok(&dir, &["train", "--input", "log.jsonl", "--id", "test:cli:tls", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "24", "--output", "events.kvf"]);

    let plant = authority(&dir, "plant-ca");
    let elsewhere = authority(&dir, "other-ca");
    plant.issue(&dir, "server", "localhost");
    plant.issue(&dir, "listed", "plant-a");
    plant.issue(&dir, "unlisted", "plant-x");
    elsewhere.issue(&dir, "stranger", "plant-a");

    let listed = ok(&dir, &["peer", "fingerprint", "--cert", "listed.pem"])["fingerprint"].as_str().unwrap().to_string();
    let server = ok(&dir, &["peer", "fingerprint", "--cert", "server.pem"])["fingerprint"].as_str().unwrap().to_string();
    assert!(listed.starts_with("sha256:") && listed.len() == 71 && listed != server);

    let tls_serve = ["--tls-cert", "server.pem", "--tls-key", "server.key", "--tls-client-ca", "plant-ca.pem", "--allow", &listed];
    let client = |cert: &'static str, key: &'static str| vec!["--tls-cert", cert, "--tls-key", key, "--tls-ca", "plant-ca.pem", "--server-name", "localhost"];

    // 1. The listed peer: the vocabulary crosses, the message arrives, the
    //    receipt matches, and each side reports who the other proved to be.
    let address = free_address();
    let listener = serve(&dir, &address, &tls_serve);
    let mut args = client("listed.pem", "listed.key");
    args.extend_from_slice(&["--server-fingerprint", &server]);
    let out = send(&dir, &address, &args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let sent: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sent["vocabulary_transferred"], true);
    assert_eq!(sent["sent"][0]["receipt_matches"], true);
    assert_eq!(sent["server_identity"], server.as_str());
    let served = listener.wait_with_output().unwrap();
    assert!(served.status.success(), "{}", String::from_utf8_lossy(&served.stderr));
    assert!(!String::from_utf8_lossy(&served.stderr).contains("no TLS"));
    let served: Value = serde_json::from_slice(&served.stdout).unwrap();
    assert_eq!(served["peer_identity"], listed.as_str());
    assert_eq!(served["vocabularies_received"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read(dir.join("received/message-1.bin")).unwrap(), std::fs::read(dir.join("message.jsonl")).unwrap());
    std::fs::remove_dir_all(dir.join("received")).unwrap();

    // 2. A certificate from the same authority that is not listed, one from
    //    another authority, no certificate at all, and a connection that
    //    says nothing: each is refused, nothing is received, and the
    //    listener is still there for the listed peer afterwards.
    let address = free_address();
    let mut with_timeout = tls_serve.to_vec();
    with_timeout.extend_from_slice(&["--timeout", "2"]);
    let listener = serve(&dir, &address, &with_timeout);
    for (what, args) in [("unlisted", client("unlisted.pem", "unlisted.key")), ("stranger", client("stranger.pem", "stranger.key")), ("no TLS", vec!["--insecure-plain"])] {
        let out = send(&dir, &address, &args);
        assert!(!out.status.success(), "{what}: the sender was served");
        assert!(!dir.join("received").exists(), "{what}: something was received");
    }
    let silent = std::net::TcpStream::connect(&address).unwrap();
    let out = send(&dir, &address, &client("listed.pem", "listed.key"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    drop(silent);
    let served = listener.wait_with_output().unwrap();
    assert!(served.status.success(), "{}", String::from_utf8_lossy(&served.stderr));
    let why = String::from_utf8_lossy(&served.stderr).to_string();
    assert!(why.contains("not authorized") && why.matches("refused ").count() == 4, "{why}");
    let served: Value = serde_json::from_slice(&served.stdout).unwrap();
    assert_eq!(served["peer_identity"], listed.as_str());
    assert!(dir.join("received/message-1.bin").exists());
    std::fs::remove_dir_all(dir.join("received")).unwrap();

    // 3. The sender checks the server as well: the wrong fingerprint, and a
    //    server certificate from an authority the sender does not trust.
    let address = free_address();
    let listener = serve(&dir, &address, &tls_serve);
    let mut args = client("listed.pem", "listed.key");
    args.extend_from_slice(&["--server-fingerprint", &listed]);
    let out = send(&dir, &address, &args);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--server-fingerprint"), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = listener.wait_with_output();
    assert!(!dir.join("received").exists());

    // 4. Asking for TLS without naming anyone is refused at once. A plain
    //    socket has to be asked for by name on both sides, and the listener
    //    says on stderr that it is one.
    for args in [vec!["peer", "serve", "--listen", "127.0.0.1:0"], vec!["peer", "send", "--connect", "127.0.0.1:9", "--vocab", "events.kvf"]] {
        let out = kute(&dir, &args);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--insecure-plain"), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let out = kute(&dir, &["peer", "serve", "--listen", &free_address(), "--tls-cert", "server.pem", "--tls-key", "server.key", "--tls-client-ca", "plant-ca.pem"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow"));
    let address = free_address();
    let listener = serve(&dir, &address, &["--insecure-plain"]);
    assert!(send(&dir, &address, &["--insecure-plain"]).status.success());
    let served = listener.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&served.stderr).contains("no TLS"));
    let served: Value = serde_json::from_slice(&served.stdout).unwrap();
    assert_eq!(served["peer_identity"], Value::Null);

    let _ = std::fs::remove_dir_all(&dir);
}
