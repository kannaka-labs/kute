//! MVP criterion: CLI training, encoding and decoding work.
//!
//! Drives the real `kute` binary through the five documented use cases
//! (docs/use-cases.md). If a command in that document changes, change it here.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn kute(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().expect("run kute")
}

fn ok(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = kute(dir, args);
    assert!(out.status.success(), "kute {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("kute {args:?} did not print JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let out = kute(dir, args);
    assert!(!out.status.success(), "kute {args:?} should have failed");
    assert!(out.stdout.is_empty(), "a failed command must not print a result");
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let report = ok(&dir, &["sample-data", "--output", "data"]);
    assert_eq!(report["synthetic"], true);
    dir
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap()
}

#[test]
fn use_case_1_text() {
    let dir = workdir("text");
    let trained = ok(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:words", "--vocab-size", "2000", "--embed-test-vectors", "--output", "words.kvf"]);
    assert_eq!(trained["summary"]["algorithm"], "bpe");
    assert_eq!(trained["summary"]["tokens"], 2000);
    let encoded = ok(&dir, &["encode", "--vocab", "words.kvf", "--input", "data/words-eval.txt", "--output", "words-eval.kute"]);
    assert!(encoded["tokens"].as_u64().unwrap() * 2 < encoded["input_bytes"].as_u64().unwrap());
    ok(&dir, &["decode", "--vocab", "words.kvf", "--input", "words-eval.kute", "--output", "words-eval.out"]);
    assert_eq!(read(&dir, "words-eval.out"), read(&dir, "data/words-eval.txt"));
    // decode to stdout is the same bytes
    let out = kute(&dir, &["decode", "--vocab", "words.kvf", "--input", "words-eval.kute"]);
    assert_eq!(out.stdout, read(&dir, "data/words-eval.txt"));

    // the compressed container is smaller and decodes to the same bytes
    let packed = ok(&dir, &["encode", "--vocab", "words.kvf", "--input", "data/words-eval.txt", "--output", "words-eval.packed.kute", "--compress"]);
    assert_eq!(packed["tokens"], encoded["tokens"]);
    assert!(packed["serialized_bytes"].as_u64().unwrap() < encoded["serialized_bytes"].as_u64().unwrap());
    ok(&dir, &["decode", "--vocab", "words.kvf", "--input", "words-eval.packed.kute", "--output", "words-eval.packed.out"]);
    assert_eq!(read(&dir, "words-eval.packed.out"), read(&dir, "data/words-eval.txt"));

    let validated = ok(&dir, &["validate", "words.kvf"]);
    assert_eq!(validated["failures"].as_array().unwrap().len(), 0);
    assert!(validated["vectors"].as_u64().unwrap() >= 1);
    let inspected = ok(&dir, &["inspect", "words.kvf", "--longest", "3"]);
    assert_eq!(inspected["summary"]["id"], "example:words");
    assert_eq!(inspected["longest_learned_tokens"].as_array().unwrap().len(), 3);
    let token = ok(&dir, &["inspect", "words.kvf", "--token", "300"]);
    assert_eq!(token["token_id"], 300);
    assert_eq!(token["vocabulary_id"], "example:words");
    assert!(token["content_hash"].as_str().unwrap().starts_with("sha256:"));

    let bench = ok(&dir, &["benchmark", "--vocab", "words.kvf", "--dataset", "data/words-eval.txt", "--iterations", "1"]);
    assert_eq!(bench["evaluation"]["round_trip_raw_exact"], true);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn use_case_2_binary() {
    let dir = workdir("binary");
    ok(&dir, &["train", "--input", "data/frames-train.bin", "--id", "example:frames", "--adapter", "binary", "--vocab-size", "600", "--output", "frames.kvf"]);
    ok(&dir, &["encode", "--vocab", "frames.kvf", "--input", "data/frames-eval.bin", "--output", "frames.kute"]);
    ok(&dir, &["decode", "--vocab", "frames.kvf", "--input", "frames.kute", "--output", "frames.out"]);
    assert_eq!(read(&dir, "frames.out"), read(&dir, "data/frames-eval.bin"));
    // The kute binary itself is as arbitrary as binary input gets.
    std::fs::copy(env!("CARGO_BIN_EXE_kute"), dir.join("kute.bin")).unwrap();
    ok(&dir, &["encode", "--vocab", "frames.kvf", "--input", "kute.bin", "--output", "kute.kute"]);
    ok(&dir, &["decode", "--vocab", "frames.kvf", "--input", "kute.kute", "--output", "kute.out"]);
    assert_eq!(read(&dir, "kute.out"), read(&dir, "kute.bin"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn use_case_3_agent_messages() {
    let dir = workdir("messages");
    let trained = ok(&dir, &["train", "--input", "data/messages-train.jsonl", "--id", "example:agent:messages", "--adapter", "jsonl", "--vocab-size", "1200", "--output", "messages.kvf"]);
    assert_eq!(trained["summary"]["adapter"], "jsonl");
    assert_eq!(trained["summary"]["adapter_lossless"], false);
    ok(&dir, &["encode", "--vocab", "messages.kvf", "--input", "data/messages-eval.jsonl", "--output", "messages.kute"]);
    ok(&dir, &["decode", "--vocab", "messages.kvf", "--input", "messages.kute", "--output", "messages.out"]);
    let parse = |b: Vec<u8>| -> Vec<serde_json::Value> { String::from_utf8(b).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect() };
    assert_eq!(parse(read(&dir, "messages.out")), parse(read(&dir, "data/messages-eval.jsonl")));
    let bench = ok(&dir, &["benchmark", "--vocab", "messages.kvf", "--dataset", "data/messages-eval.jsonl", "--iterations", "1"]);
    assert_eq!(bench["evaluation"]["round_trip_canonical_exact"], true);
    assert_eq!(bench["evaluation"]["round_trip_raw_exact"], false);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn use_case_4_industrial_events() {
    let dir = workdir("events");
    let trained = ok(&dir, &[
        "train", "--input", "data/alarms-train.jsonl", "--id", "kute:industrial:events", "--algorithm", "sequence-bpe",
        "--adapter", "jsonl-events", "--adapter-option", "field=tag", "--vocab-size", "64", "--output", "industrial.kvf",
    ]);
    assert_eq!(trained["summary"]["payload_kind"], "symbols");
    let inspected = ok(&dir, &["inspect", "industrial.kvf", "--longest", "5"]);
    let longest = inspected["longest_learned_tokens"].as_array().unwrap();
    assert!(longest.iter().any(|t| t["payload"].as_array().is_some_and(|p| p.len() >= 3)), "expected composite event tokens");
    let ids = ok(&dir, &["encode", "--vocab", "industrial.kvf", "--input", "data/alarms-eval.jsonl"]);
    assert_eq!(ids["vocabulary_id"], "kute:industrial:events");
    assert!(ids["ids"].as_array().unwrap().len() > 100);
    // A frozen vocabulary meets an event it has never seen.
    let mut novel = read(&dir, "data/alarms-eval.jsonl");
    novel.extend_from_slice(b"{\"tag\":\"VALVE_7_STUCK\"}\n");
    std::fs::write(dir.join("novel.jsonl"), &novel).unwrap();
    let refused = fails(&dir, &["encode", "--vocab", "industrial.kvf", "--input", "novel.jsonl", "--output", "novel.kute"]);
    assert!(refused.contains("VALVE_7_STUCK"), "{refused}");
    let escaped = ok(&dir, &["encode", "--vocab", "industrial.kvf", "--input", "novel.jsonl", "--output", "novel.kute", "--unknown", "escape", "--compress"]);
    assert_eq!(escaped["escaped_symbols"], 1);
    let over = fails(&dir, &["decode", "--vocab", "industrial.kvf", "--input", "novel.kute", "--output", "novel.out", "--max-units", "5"]);
    assert!(over.contains("limit exceeded"), "{over}");
    ok(&dir, &["decode", "--vocab", "industrial.kvf", "--input", "novel.kute", "--output", "novel.out"]);
    let decoded = String::from_utf8(read(&dir, "novel.out")).unwrap();
    assert!(decoded.trim_end().ends_with("VALVE_7_STUCK\"}"), "the unseen event must come back");
    // A private corpus: no excerpts in the published file.
    ok(&dir, &[
        "train", "--input", "data/alarms-train.jsonl", "--id", "kute:industrial:private", "--algorithm", "sequence-bpe",
        "--adapter", "jsonl-events", "--adapter-option", "field=tag", "--vocab-size", "64", "--output", "private.kvf",
    ]);
    let private: serde_json::Value = serde_json::from_slice(&read(&dir, "private.kvf")).unwrap();
    assert_eq!(private["test_vectors"].as_array().unwrap().len(), 0);
    // Discovery needs no vocabulary: patterns with evidence, checked on the
    // evaluation file and against a shuffled control.
    let found = ok(&dir, &[
        "discover", "--input", "data/alarms-train.jsonl", "--adapter-option", "field=tag",
        "--holdout", "data/alarms-eval.jsonl", "--control", "--top", "10", "--output", "patterns.json",
    ]);
    let patterns = found["discovery"]["patterns"].as_array().unwrap();
    assert!(!patterns.is_empty());
    assert!(patterns[0]["support"].as_u64().unwrap() >= 5);
    assert!(patterns[0]["examples"].as_array().is_some_and(|e| !e.is_empty()));
    assert!(found["discovery"]["observation"]["fingerprint"].as_str().unwrap().starts_with("sha256:"));
    assert!(found["held_out"]["confirmed"].as_u64().unwrap() >= 1);
    assert!(found["control"]["coverage"].as_f64().unwrap() < found["discovery"]["coverage"].as_f64().unwrap());
    assert!(dir.join("patterns.json").exists());
    // Evolution: the unseen event becomes a token of the next version, and
    // every old id keeps its meaning.
    let evolved = ok(&dir, &[
        "evolve", "--vocab", "industrial.kvf", "--add", "VALVE_7_STUCK", "--new-version", "0.2.0", "--output", "industrial-0.2.kvf",
    ]);
    assert_eq!(evolved["summary"]["tokens"].as_u64().unwrap(), evolved["parent"]["tokens"].as_u64().unwrap() + 1);
    ok(&dir, &["validate", "industrial-0.2.kvf"]);
    // The compact form of the same vocabulary: same digest, smaller, and
    // every command reads it in place of the JSON file.
    let converted = ok(&dir, &["convert", "--input", "industrial-0.2.kvf", "--output", "industrial-0.2.kvfb"]);
    assert_eq!(converted["digest"], evolved["summary"]["digest"]);
    assert!(converted["output"]["bytes"].as_u64().unwrap() < converted["input"]["bytes"].as_u64().unwrap());
    ok(&dir, &["validate", "industrial-0.2.kvfb"]);
    ok(&dir, &["encode", "--vocab", "industrial-0.2.kvfb", "--input", "novel.jsonl", "--output", "novel-0.2b.kute"]);
    assert_eq!(read(&dir, "novel-0.2b.kute"), {
        ok(&dir, &["encode", "--vocab", "industrial-0.2.kvf", "--input", "novel.jsonl", "--output", "novel-0.2a.kute"]);
        read(&dir, "novel-0.2a.kute")
    });
    let back = ok(&dir, &["convert", "--input", "industrial-0.2.kvfb", "--output", "industrial-0.2-back.kvf"]);
    assert_eq!(back["digest"], evolved["summary"]["digest"]);
    assert_eq!(read(&dir, "industrial-0.2-back.kvf"), read(&dir, "industrial-0.2.kvf"));
    ok(&dir, &["encode", "--vocab", "industrial-0.2.kvf", "--input", "novel.jsonl", "--output", "novel-0.2.kute"]);
    assert!(fails(&dir, &["evolve", "--vocab", "industrial.kvf", "--add", "X", "--new-version", "0.0.1", "--output", "bad.kvf"]).contains("greater"));
    // Two processes: one serves with no vocabulary, the other shares its
    // vocabulary, proposes the new event and sends a file.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let address = format!("127.0.0.1:{port}");
    let server = std::process::Command::new(env!("CARGO_BIN_EXE_kute"))
        .current_dir(&dir)
        .args(["peer", "serve", "--listen", &address, "--accept-vocabularies", "--accept-proposals", "--output-dir", "received"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut sent = None;
    for _ in 0..100 {
        let out = kute(&dir, &[
            "peer", "send", "--connect", &address, "--vocab", "industrial.kvf", "--input", "novel.jsonl",
            "--propose", "VALVE_7_STUCK", "--new-version", "0.2.0",
        ]);
        if out.status.success() {
            sent = Some(serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let sent = sent.expect("the sender never connected");
    assert_eq!(sent["vocabulary_transferred"], true);
    assert_eq!(sent["sent"][0]["receipt_matches"], true);
    let served = server.wait_with_output().unwrap();
    assert!(served.status.success(), "{}", String::from_utf8_lossy(&served.stderr));
    let served: serde_json::Value = serde_json::from_slice(&served.stdout).unwrap();
    assert_eq!(served["vocabularies_received"].as_array().unwrap().len(), 2);
    assert_eq!(served["received"][0]["escaped_symbols"], 0);
    // Both processes derived the same evolved vocabulary as `kute evolve` did.
    assert_eq!(sent["vocabulary_after_proposal"], evolved["summary"]["digest"]);
    let received = String::from_utf8(read(&dir, "received/message-1.bin")).unwrap();
    assert!(received.trim_end().ends_with("VALVE_7_STUCK\"}"));
    // Typed events: two valves interleaved in one file, with times. Grouped,
    // each valve shows OPEN then CLOSE; the report says how long that takes.
    let mut lines = String::new();
    for i in 0..20 {
        for (valve, held) in [("v1", 5), ("v2", 9)] {
            lines.push_str(&format!("{{\"valve\":\"{valve}\",\"tag\":\"OPEN\",\"ms\":{}}}\n", i * 100));
            lines.push_str(&format!("{{\"valve\":\"{valve}\",\"tag\":\"CLOSE\",\"ms\":{}}}\n", i * 100 + held));
        }
    }
    std::fs::write(dir.join("valves.jsonl"), lines).unwrap();
    let timed = ok(&dir, &[
        "discover", "--input", "valves.jsonl", "--symbol-field", "tag", "--group-field", "valve", "--time-field", "ms",
        "--time-unit", "ms", "--schema-id", "kute:test:valves",
    ]);
    assert_eq!(timed["events"]["searched"]["records"], 2);
    assert_eq!(timed["events"]["schema"]["time_unit"], "milliseconds");
    let patterns = timed["discovery"]["patterns"].as_array().unwrap();
    let open_close = patterns.iter().position(|p| p["kind"] == "contiguous" && p["symbols"] == serde_json::json!(["OPEN", "CLOSE"])).unwrap();
    let duration = &timed["events"]["timing"][open_close]["duration"];
    assert_eq!((duration["occurrences"].as_u64(), duration["min"].as_i64(), duration["max"].as_i64()), (Some(40), Some(5), Some(9)));
    assert!(fails(&dir, &["discover", "--input", "valves.jsonl", "--symbol-field", "tag", "--time-field", "nope"]).contains("nope"));
    // Dictionary baseline over the same events, then compare the two.
    ok(&dir, &[
        "train", "--input", "data/alarms-train.jsonl", "--id", "kute:industrial:event-names", "--algorithm", "dictionary",
        "--adapter", "jsonl-events", "--adapter-option", "field=tag", "--vocab-size", "64", "--output", "names.kvf",
    ]);
    let compared = ok(&dir, &["compare", "industrial.kvf", "names.kvf"]);
    assert_eq!(compared["identical_content"], false);
    assert_eq!(compared["only_in_b"], 0);
    assert!(compared["shared_payloads"].as_u64().unwrap() >= 10);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn use_case_5_tiktoken_export() {
    let dir = workdir("export");
    ok(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:export", "--vocab-size", "1000", "--reserved", "<|endoftext|>", "--output", "export.kvf"]);
    let exported = ok(&dir, &["export", "--vocab", "export.kvf", "--format", "tiktoken", "--output", "export.tiktoken"]);
    assert_eq!(exported["tokens"], 1000);
    let lines = String::from_utf8(read(&dir, "export.tiktoken")).unwrap().lines().count();
    assert_eq!(lines, 999);
    let meta: serde_json::Value = serde_json::from_slice(&read(&dir, "export.tiktoken.json")).unwrap();
    assert_eq!(meta["special_tokens"]["<|endoftext|>"], 999);
    assert_eq!(meta["explicit_n_vocab"], 1000);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn bad_input_fails_cleanly() {
    let dir = workdir("errors");
    ok(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:a", "--vocab-size", "400", "--output", "a.kvf"]);
    ok(&dir, &["train", "--input", "data/frames-train.bin", "--id", "example:b", "--adapter", "binary", "--vocab-size", "400", "--output", "b.kvf"]);
    ok(&dir, &["encode", "--vocab", "a.kvf", "--input", "data/words-eval.txt", "--output", "a.kute"]);

    assert!(fails(&dir, &["train", "--input", "data/words-train.txt", "--id", "Not Valid", "--output", "x.kvf"]).contains("invalid identifier"));
    assert!(fails(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:x", "--vocab-version", "1.0", "--output", "x.kvf"]).contains("version"));
    assert!(fails(&dir, &["train", "--input", "no-such-file.txt", "--id", "example:x", "--output", "x.kvf"]).contains("no-such-file"));
    assert!(fails(&dir, &["train", "--input", "data/frames-train.bin", "--id", "example:x", "--output", "x.kvf"]).contains("UTF-8"));
    // Same path, different content: refused without --force.
    assert!(fails(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:a", "--vocab-size", "500", "--output", "a.kvf"]).contains("immutable"));
    ok(&dir, &["train", "--input", "data/words-train.txt", "--id", "example:a", "--vocab-size", "400", "--output", "a.kvf"]);
    // A sequence from one vocabulary is not decodable with another.
    assert!(fails(&dir, &["decode", "--vocab", "b.kvf", "--input", "a.kute"]).contains("vocabulary mismatch"));
    // Damaged and foreign files.
    std::fs::write(dir.join("junk.kute"), b"not a token sequence").unwrap();
    assert!(fails(&dir, &["decode", "--vocab", "a.kvf", "--input", "junk.kute"]).contains("not a valid .kute"));
    let mut damaged = String::from_utf8(read(&dir, "a.kvf")).unwrap();
    damaged = damaged.replacen("\"kind\":\"merge\"", "\"kind\":\"byte\"", 1);
    std::fs::write(dir.join("damaged.kvf"), damaged).unwrap();
    assert!(!fails(&dir, &["validate", "damaged.kvf"]).is_empty());
    std::fs::write(dir.join("truncated.kvf"), &read(&dir, "a.kvf")[..2000]).unwrap();
    assert!(fails(&dir, &["inspect", "truncated.kvf"]).contains("not a KVF document"));
    assert!(!fails(&dir, &["export", "--vocab", "a.kvf", "--output", "data"]).is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn capabilities_lists_everything_registered() {
    let dir = std::env::temp_dir();
    let caps = ok(&dir, &["capabilities"]);
    let names = |key: &str, field: &str| -> Vec<String> { caps[key].as_array().unwrap().iter().map(|a| a[field].as_str().unwrap().to_string()).collect() };
    for algorithm in ["bpe", "bytes", "char", "dictionary", "sequence-bpe"] {
        assert!(names("algorithms", "algorithm").contains(&algorithm.to_string()), "{algorithm}");
    }
    for adapter in ["binary", "text", "json", "jsonl", "jsonl-events"] {
        assert!(names("adapters", "name").contains(&adapter.to_string()), "{adapter}");
    }
    // Every adapter and algorithm states whether it is lossless.
    assert!(caps["adapters"].as_array().unwrap().iter().all(|a| a["lossless"].is_boolean()));
    assert!(caps["algorithms"].as_array().unwrap().iter().all(|a| a["lossless"].is_boolean()));
}
