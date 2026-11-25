//! `kute peer serve --require-signature-from`: a vocabulary from the
//! other side is taken only if a trusted key signed exactly it.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

/// One connection: a server that requires a signature from `trusted`,
/// and a sender offering `vocab`. Returns the sender's output and what
/// the server received.
fn exchange(dir: &Path, name: &str, trusted: &[&str], vocab: &str) -> (Output, Vec<PathBuf>) {
    // The server picks the port and says which: choosing one here and
    // letting it go would leave a moment for another test to take it.
    let mut args = vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--output-dir", name, "--timeout", "20"];
    for key in trusted {
        args.extend_from_slice(&["--require-signature-from", key]);
    }
    let mut server = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // It says when it is listening; send only then.
    let mut line = String::new();
    BufReader::new(server.stderr.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("listening"), "{line}");
    let address = line.trim().rsplit(' ').next().unwrap().to_string();
    let sent = kute(dir, &["peer", "send", "--connect", &address, "--insecure-plain", "--vocab", vocab, "--input", "log.jsonl"]);
    let _ = server.wait();
    let mut received: Vec<PathBuf> = std::fs::read_dir(dir.join(name)).map(|d| d.map(|e| e.unwrap().path()).collect()).unwrap_or_default();
    received.sort();
    (sent, received)
}

#[test]
fn a_peer_takes_only_a_vocabulary_signed_by_a_key_it_trusts() {
    let dir = scratch("peer-signed");
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    for (id, size) in [("test:peer:signed", "8"), ("test:peer:other", "7")] {
        let file = format!("{}.kvf", id.rsplit(':').next().unwrap());
        ok(&dir, &["train", "--input", "log.jsonl", "--id", id, "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", size, "--output", &file]);
    }
    let ours = ok(&dir, &["keygen", "--output", "ours.pem"])["public_key"].as_str().unwrap().to_string();
    let theirs = ok(&dir, &["keygen", "--output", "theirs.pem"])["public_key"].as_str().unwrap().to_string();
    ok(&dir, &["sign", "--vocab", "signed.kvf", "--key", "ours.pem", "--output", "by-ours.kvf"]);
    ok(&dir, &["sign", "--vocab", "signed.kvf", "--key", "theirs.pem", "--output", "by-theirs.kvf"]);

    // Signed by the trusted key: taken, and the message under it arrives.
    let (sent, received) = exchange(&dir, "r-ours", &[&ours], "by-ours.kvf");
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    let report: Value = serde_json::from_slice(&sent.stdout).unwrap();
    assert_eq!((report["vocabulary_transferred"].clone(), report["sent"][0]["receipt_matches"].clone()), (true.into(), true.into()));
    assert!(received.iter().any(|p| p.extension().is_some_and(|e| e == "kvf")), "{received:?}");
    // What the receiver kept still carries the signature and verifies.
    let kept = received.iter().find(|p| p.extension().is_some_and(|e| e == "kvf")).unwrap();
    assert_eq!(ok(&dir, &["verify", "--vocab", kept.to_str().unwrap(), "--trusted-key", &ours])["verified"], true);
    // One trusted key of two is enough.
    assert!(exchange(&dir, "r-either", &[&theirs, &ours], "by-ours.kvf").0.status.success());

    // Not signed, signed by someone else, or carrying a signature made
    // for another vocabulary: not taken, nothing kept, and the sender is told why.
    std::fs::copy(dir.join("other.kvf"), dir.join("transplant.kvf")).unwrap();
    let mut other: Value = serde_json::from_slice(&std::fs::read(dir.join("other.kvf")).unwrap()).unwrap();
    let signed: Value = serde_json::from_slice(&std::fs::read(dir.join("by-ours.kvf")).unwrap()).unwrap();
    other["integrity"]["signatures"] = signed["integrity"]["signatures"].clone();
    std::fs::write(dir.join("transplant.kvf"), serde_json::to_vec(&other).unwrap()).unwrap();
    for (name, vocab, why) in [
        ("r-none", "signed.kvf", "carries no signature"),
        ("r-theirs", "by-theirs.kvf", "none of its signatures"),
        ("r-transplant", "transplant.kvf", "none of its signatures"),
    ] {
        let (sent, received) = exchange(&dir, name, &[&ours], vocab);
        let stderr = String::from_utf8_lossy(&sent.stderr).into_owned();
        assert!(!sent.status.success() && stderr.contains("not taken") && stderr.contains(why), "{name}: {stderr}");
        assert!(received.is_empty(), "{name}: {received:?}");
    }
    // Without the requirement the unsigned one is taken, as before.
    assert!(exchange(&dir, "r-open", &[], "signed.kvf").0.status.success());

    // The option cannot be combined with what it would make meaningless.
    for args in [
        vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--require-signature-from", ours.as_str()],
        vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--accept-proposals", "--require-signature-from", ours.as_str()],
    ] {
        assert!(!kute(&dir, &args).status.success(), "{args:?}");
    }
    let bad = kute(&dir, &["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--require-signature-from", "not-a-key"]);
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("trusted key"));
    let _ = std::fs::remove_dir_all(&dir);
}
