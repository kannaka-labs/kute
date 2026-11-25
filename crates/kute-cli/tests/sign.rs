//! `kute keygen`, `kute sign`, `kute verify`: a signature holds for the
//! exact content it was made for and for the key it is checked against,
//! and for nothing else.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> (bool, Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap();
    (out.status.success(), serde_json::from_slice(&out.stdout).unwrap_or(Value::Null), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let (success, report, stderr) = run(dir, args);
    assert!(success, "kute {args:?} failed: {stderr}");
    report
}

fn statuses(report: &Value) -> Vec<&str> {
    report["signatures"].as_array().unwrap().iter().map(|s| s["status"].as_str().unwrap()).collect()
}

/// Rewrite a KVF text file after changing its parsed form.
fn edit(path: &Path, change: impl FnOnce(&mut Value)) {
    let mut doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    change(&mut doc);
    std::fs::write(path, serde_json::to_vec(&doc).unwrap()).unwrap();
}

#[test]
fn a_signature_holds_for_its_content_and_its_key_and_nothing_else() {
    let dir = scratch("sign");
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    ok(&dir, &["train", "--input", "log.jsonl", "--id", "test:sign:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "8", "--output", "plain.kvf"]);
    let digest = ok(&dir, &["inspect", "plain.kvf"])["summary"]["digest"].as_str().unwrap().to_string();

    // A key: the private half in a file, the public half printed.
    let ours = ok(&dir, &["keygen", "--output", "ours.pem"]);
    let (public, id) = (ours["public_key"].as_str().unwrap().to_string(), ours["key_id"].as_str().unwrap().to_string());
    assert!(id.starts_with("ed25519:") && id.len() == 8 + 64);
    assert!(std::fs::read_to_string(dir.join("ours.pem")).unwrap().starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(!ours.to_string().contains("BEGIN PRIVATE"), "the private key is printed");
    // An existing key file is never written over.
    assert!(!run(&dir, &["keygen", "--output", "ours.pem"]).0);
    let other = ok(&dir, &["keygen", "--output", "other.pem"]);
    let other_public = other["public_key"].as_str().unwrap().to_string();
    assert_ne!(other_public, public);

    // Signing leaves the digest alone.
    let signed = ok(&dir, &["sign", "--vocab", "plain.kvf", "--key", "ours.pem", "--output", "signed.kvf"]);
    assert_eq!(signed["digest"], digest.as_str());
    assert_eq!(signed["digest_unchanged"], true);
    assert_eq!(signed["key_id"], id.as_str());
    assert_eq!(signed["signatures"], 1);
    assert_eq!(ok(&dir, &["inspect", "signed.kvf"])["summary"]["digest"], digest.as_str());
    ok(&dir, &["validate", "signed.kvf"]);

    // It verifies against our key, and not against someone else's.
    let report = ok(&dir, &["verify", "--vocab", "signed.kvf", "--trusted-key", &public]);
    assert_eq!(report["verified"], true);
    assert_eq!(statuses(&report), ["valid"]);
    let (success, report, stderr) = run(&dir, &["verify", "--vocab", "signed.kvf", "--trusted-key", &other_public]);
    assert!(!success);
    assert_eq!(report["verified"], false);
    assert_eq!(statuses(&report), ["not from a trusted key"]);
    assert!(stderr.contains("no trusted key has signed"));
    // An unsigned vocabulary verifies against nothing.
    let (success, report, _) = run(&dir, &["verify", "--vocab", "plain.kvf", "--trusted-key", &public]);
    assert!(!success);
    assert_eq!(report["signatures"], serde_json::json!([]));
    // And verifying without saying whom you trust is refused.
    assert!(!run(&dir, &["verify", "--vocab", "signed.kvf"]).0);
    assert!(!run(&dir, &["verify", "--vocab", "signed.kvf", "--trusted-key", "not base64!"]).0);
    assert!(!run(&dir, &["verify", "--vocab", "signed.kvf", "--trusted-key", "AAAA"]).0);

    // The compact form carries the signature.
    ok(&dir, &["convert", "--input", "signed.kvf", "--output", "signed.kvfb"]);
    assert_eq!(ok(&dir, &["verify", "--vocab", "signed.kvfb", "--trusted-key", &public])["verified"], true);

    // A second key adds a second signature; the same key again adds nothing.
    let twice = ok(&dir, &["sign", "--vocab", "signed.kvf", "--key", "other.pem", "--output", "both.kvf"]);
    assert_eq!(twice["signatures"], 2);
    let again = ok(&dir, &["sign", "--vocab", "both.kvf", "--key", "ours.pem", "--output", "both-again.kvf"]);
    assert_eq!(again["already_signed_by_this_key"], true);
    assert_eq!(again["signatures"], 2);
    let report = ok(&dir, &["verify", "--vocab", "both.kvf", "--trusted-key", &other_public]);
    assert_eq!(statuses(&report), ["not from a trusted key", "valid"]);
    assert_eq!(statuses(&ok(&dir, &["verify", "--vocab", "both.kvf", "--trusted-key", &public, "--trusted-key", &other_public])), ["valid", "valid"]);

    // A signature that has been altered is invalid.
    std::fs::copy(dir.join("signed.kvf"), dir.join("bent.kvf")).unwrap();
    edit(&dir.join("bent.kvf"), |doc| {
        let s = doc["integrity"]["signatures"][0]["signature"].as_str().unwrap().to_string();
        let flipped = if s.starts_with('A') { "B" } else { "A" };
        doc["integrity"]["signatures"][0]["signature"] = Value::String(format!("{flipped}{}", &s[1..]));
    });
    let (success, report, _) = run(&dir, &["verify", "--vocab", "bent.kvf", "--trusted-key", &public]);
    assert!(!success);
    assert_eq!(statuses(&report), ["INVALID"]);

    // A signature lifted onto another vocabulary is invalid there.
    ok(&dir, &["evolve", "--vocab", "plain.kvf", "--add", "OPEN READ CLOSE SYNC OPEN", "--new-version", "0.2.0", "--output", "child.kvf"]);
    let lifted = serde_json::from_slice::<Value>(&std::fs::read(dir.join("signed.kvf")).unwrap()).unwrap()["integrity"]["signatures"].clone();
    edit(&dir.join("child.kvf"), |doc| doc["integrity"]["signatures"] = lifted);
    let (success, report, _) = run(&dir, &["verify", "--vocab", "child.kvf", "--trusted-key", &public]);
    assert!(!success);
    assert_eq!(statuses(&report), ["INVALID"]);

    // Content changed under a signature: the file no longer loads at all.
    std::fs::copy(dir.join("signed.kvf"), dir.join("changed.kvf")).unwrap();
    edit(&dir.join("changed.kvf"), |doc| doc["manifest"]["description"] = Value::String("something else".into()));
    let (success, _, stderr) = run(&dir, &["verify", "--vocab", "changed.kvf", "--trusted-key", &public]);
    assert!(!success);
    assert!(stderr.contains("does not match content digest"), "{stderr}");

    // A child made from a signed vocabulary is new content: it starts unsigned.
    ok(&dir, &["evolve", "--vocab", "signed.kvf", "--add", "OPEN READ CLOSE SYNC OPEN", "--new-version", "0.2.0", "--output", "child-of-signed.kvf"]);
    let (success, report, _) = run(&dir, &["verify", "--vocab", "child-of-signed.kvf", "--trusted-key", &public]);
    assert!(!success);
    assert_eq!(report["signatures"], serde_json::json!([]));

    // A file stuffed with signatures is refused, as is something that is not a key.
    std::fs::copy(dir.join("signed.kvf"), dir.join("stuffed.kvf")).unwrap();
    edit(&dir.join("stuffed.kvf"), |doc| {
        let one = doc["integrity"]["signatures"][0].clone();
        doc["integrity"]["signatures"] = Value::Array(vec![one; 17]);
    });
    assert!(!run(&dir, &["verify", "--vocab", "stuffed.kvf", "--trusted-key", &public]).0);
    std::fs::write(dir.join("junk.pem"), "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n").unwrap();
    let (success, _, stderr) = run(&dir, &["sign", "--vocab", "plain.kvf", "--key", "junk.pem", "--output", "x.kvf"]);
    assert!(!success && stderr.contains("not a usable signing key"), "{stderr}");
    assert!(!dir.join("x.kvf").exists());

    // Signed or not, a vocabulary encodes the same.
    std::fs::write(dir.join("m.jsonl"), "{\"event\":\"OPEN\"}\n{\"event\":\"READ\"}\n").unwrap();
    ok(&dir, &["encode", "--vocab", "plain.kvf", "--input", "m.jsonl", "--output", "a.kute"]);
    ok(&dir, &["encode", "--vocab", "signed.kvf", "--input", "m.jsonl", "--output", "b.kute"]);
    assert_eq!(std::fs::read(dir.join("a.kute")).unwrap(), std::fs::read(dir.join("b.kute")).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}
