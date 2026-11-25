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
    exchange_with(dir, name, trusted, vocab, &[], &[])
}

/// [`exchange`] with more options for the server and for the sender.
fn exchange_with(dir: &Path, name: &str, trusted: &[&str], vocab: &str, serve: &[&str], send: &[&str]) -> (Output, Vec<PathBuf>) {
    // The server picks the port and says which.
    let mut args = vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--output-dir", name, "--timeout", "20"];
    for key in trusted {
        args.extend_from_slice(&["--require-signature-from", key]);
    }
    args.extend_from_slice(serve);
    let mut server = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // It says when it is listening; send only then.
    // The reader is kept until the server has ended. Dropping it closes
    // the pipe, and the server's next line to it (the warning about no
    // TLS follows at once) then ends the server before it accepts: about
    // one run in fifteen, seen as "connection refused" by the sender.
    let mut line = String::new();
    let mut errors = BufReader::new(server.stderr.take().unwrap());
    errors.read_line(&mut line).unwrap();
    assert!(line.contains("listening"), "{line}");
    let address = line.trim().rsplit(' ').next().unwrap().to_string();
    let mut sending = vec!["peer", "send", "--connect", &address, "--insecure-plain", "--vocab", vocab, "--input", "log.jsonl"];
    sending.extend_from_slice(send);
    let sent = kute(dir, &sending);
    let _ = server.wait();
    drop(errors);
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

    // A proposal to such a peer: the proposer signs the vocabulary the
    // proposal derives and sends the signature alone. The peer derives
    // the vocabulary itself and keeps it only if the signature holds.
    let serve = ["--accept-proposals", "--vocab", "signed.kvf"];
    let propose = ["--propose", "SYNC OPEN", "--new-version", "0.2.0"];
    let derived = |received: &[PathBuf]| received.iter().filter(|p| p.extension().is_some_and(|e| e == "kvf")).cloned().collect::<Vec<_>>();
    let (sent, received) = exchange_with(&dir, "p-ours", &[&ours], "signed.kvf", &serve, &[&propose[..], &["--sign-with", "ours.pem"]].concat());
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    let report: Value = serde_json::from_slice(&sent.stdout).unwrap();
    assert_eq!((report["proposal_signed"].clone(), report["sent"][0]["receipt_matches"].clone()), (true.into(), true.into()));
    let kept = derived(&received);
    assert_eq!(kept.len(), 1, "{received:?}");
    let verified = ok(&dir, &["verify", "--vocab", kept[0].to_str().unwrap(), "--trusted-key", &ours]);
    assert_eq!((verified["verified"].clone(), verified["digest"].clone()), (true.into(), report["vocabulary_after_proposal"].clone()));
    // The same derivation made here and signed with `kute sign` is the same file content.
    ok(&dir, &["evolve", "--vocab", "signed.kvf", "--add", "SYNC OPEN", "--new-version", "0.2.0", "--output", "child.kvf"]);
    assert_eq!(ok(&dir, &["inspect", "child.kvf"])["summary"]["digest"], report["vocabulary_after_proposal"]);
    // Unsigned, or signed by a key the peer was not told of: refused, nothing derived is kept, no message delivered.
    for (name, extra, why) in [("p-none", vec![], "carries no signature"), ("p-theirs", vec!["--sign-with", "theirs.pem"], "none of its signatures")] {
        let (sent, received) = exchange_with(&dir, name, &[&ours], "signed.kvf", &serve, &[&propose[..], &extra[..]].concat());
        let stderr = String::from_utf8_lossy(&sent.stderr).into_owned();
        assert!(!sent.status.success() && stderr.contains(why), "{name}: {stderr}");
        assert!(received.is_empty(), "{name}: {received:?}");
    }
    // A key that cannot be read is told before connecting: nothing
    // listens at this address, and the complaint is about the key.
    let sent = kute(&dir, &["peer", "send", "--connect", "127.0.0.1:9", "--insecure-plain", "--vocab", "signed.kvf", "--propose", "SYNC OPEN", "--new-version", "0.2.0", "--sign-with", "absent.pem"]);
    let stderr = String::from_utf8_lossy(&sent.stderr).into_owned();
    assert!(!sent.status.success() && stderr.contains("absent.pem"), "{stderr}");

    // The option cannot be used without what it guards.
    let args = vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--require-signature-from", ours.as_str()];
    assert!(!kute(&dir, &args).status.success(), "{args:?}");
    let bad = kute(&dir, &["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--require-signature-from", "not-a-key"]);
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("trusted key"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A parent that stops reading the server's standard error must not be
/// able to stop the server: its later lines there are best effort.
#[test]
fn a_server_whose_standard_error_was_closed_still_serves() {
    let dir = scratch("peer-closed-stderr");
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}
", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    ok(&dir, &["train", "--input", "log.jsonl", "--id", "test:peer:closed", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "8", "--output", "v.kvf"]);
    for round in 0..8 {
        let out = format!("r{round}");
        let mut server = Command::new(env!("CARGO_BIN_EXE_kute"))
            .current_dir(&dir)
            .args(["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--output-dir", &out, "--timeout", "20"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        // Read one line and let the pipe go: the warning that follows has nowhere to be written.
        BufReader::new(server.stderr.take().unwrap()).read_line(&mut line).unwrap();
        let address = line.trim().rsplit(' ').next().unwrap().to_string();
        let sent = kute(&dir, &["peer", "send", "--connect", &address, "--insecure-plain", "--vocab", "v.kvf", "--input", "log.jsonl"]);
        let status = server.wait().unwrap();
        assert!(sent.status.success(), "round {round}: {}", String::from_utf8_lossy(&sent.stderr));
        assert!(status.success(), "round {round}: the server ended with {status}");
    }
}
