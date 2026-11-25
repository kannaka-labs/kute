//! `kute shadow`: a candidate vocabulary is approved only when it pays
//! for itself on data neither vocabulary was made from. Synthetic data.

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
    let report = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.success(), report, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let (success, report, stderr) = run(dir, args);
    assert!(success, "kute {args:?} failed: {stderr}");
    report
}

/// Events of eight kinds in no order; with `cycle`, a fixed run of four
/// of them appears between the others about a third of the time.
fn events(seed: u64, count: usize, cycle: bool) -> String {
    let mut state = seed;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let names = ["OPEN", "CLOSE", "READ", "WRITE", "SYNC", "LOCK", "FREE", "PING"];
    let mut out = String::new();
    let mut written = 0;
    while written < count {
        if cycle && next(3) == 0 {
            for name in ["LOCK", "WRITE", "SYNC", "FREE"] {
                out.push_str(&format!("{{\"event\":\"{name}\"}}\n"));
            }
            written += 4;
        } else {
            out.push_str(&format!("{{\"event\":\"{}\"}}\n", names[next(8) as usize]));
            written += 1;
        }
    }
    out
}

#[test]
fn a_candidate_is_approved_only_when_it_pays_for_itself() {
    let dir = scratch("shadow");
    // The vocabulary in use was made before the cycle existed.
    std::fs::write(dir.join("early.jsonl"), events(1, 4000, false)).unwrap();
    std::fs::write(dir.join("held-1.jsonl"), events(2, 30_000, true)).unwrap();
    std::fs::write(dir.join("held-2.jsonl"), events(3, 30_000, true)).unwrap();
    ok(&dir, &["train", "--input", "early.jsonl", "--id", "test:shadow:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "8", "--output", "parent.kvf"]);
    ok(&dir, &["evolve", "--vocab", "parent.kvf", "--add", "LOCK WRITE SYNC FREE", "--new-version", "0.2.0", "--output", "cycle.kvf"]);
    ok(&dir, &["evolve", "--vocab", "parent.kvf", "--add", "NEVER_HAPPENS", "--new-version", "0.2.0", "--output", "useless.kvf"]);
    let shadow = |candidate: &str, extra: &[&str]| {
        let mut args = vec!["shadow", "--parent", "parent.kvf", "--candidate", candidate, "--input", "held-1.jsonl", "held-2.jsonl"];
        args.extend_from_slice(extra);
        run(&dir, &args)
    };

    // Carrying a thousand times the held-out data, the cycle pays for
    // its distribution many times over.
    let (success, report, stderr) = shadow("cycle.kvf", &["--horizon", "1000"]);
    assert!(success, "{stderr}");
    assert_eq!(report["decision"], "approve", "{report}");
    assert_eq!(report["reasons"], serde_json::json!([]));
    assert_eq!(report["candidate"]["lineage"], "child of the parent");
    let comparison = &report["comparison"];
    assert!(comparison["token_ratio"].as_f64().unwrap() < 0.8);
    assert!(comparison["container_ratio"].as_f64().unwrap() < 1.0);
    assert!(comparison["net_improvement"].as_f64().unwrap() >= 0.01);
    let saved = comparison["container_bytes_saved_on_inputs"].as_f64().unwrap();
    let cost = comparison["distribution_cost_bytes"].as_f64().unwrap();
    assert_eq!(cost, report["candidate"]["file_bytes"].as_f64().unwrap());
    assert!((comparison["net_bytes_saved_over_horizon"].as_f64().unwrap() - (1000.0 * saved - cost)).abs() < 1e-6);
    assert_eq!(report["inputs"]["files"], 2);
    assert_eq!(comparison["inputs_smaller"], 2);
    assert_eq!(comparison["inputs_larger"], 0);
    assert_eq!(comparison["largest_increase_on_one_input_bytes"], 0);
    assert!(report["assumes"].as_str().unwrap().contains("drift"));

    // The same candidate sent to ten thousand peers for that traffic does not.
    let (success, report, _) = shadow("cycle.kvf", &["--horizon", "1000", "--distributions", "10000"]);
    assert!(success);
    assert_eq!(report["decision"], "reject");
    assert!(report["reasons"][0].as_str().unwrap().contains("net improvement"), "{report}");
    assert!(report["comparison"]["net_bytes_saved_over_horizon"].as_f64().unwrap() < 0.0);
    // Nor does it clear a bar set higher than what it saves.
    let (_, report, _) = shadow("cycle.kvf", &["--horizon", "1000", "--min-improvement", "0.9"]);
    assert_eq!(report["decision"], "reject");

    // A token for something that never happens saves nothing, however
    // long it is used.
    let (success, report, _) = shadow("useless.kvf", &["--horizon", "1000000"]);
    assert!(success);
    assert_eq!(report["decision"], "reject");
    assert_eq!(report["comparison"]["container_bytes_saved_on_inputs"], 0.0);
    assert_eq!(report["comparison"]["break_even_horizon"], Value::Null);

    // The parent as its own candidate; and rejection as an exit status.
    let (_, report, _) = shadow("parent.kvf", &["--horizon", "1000"]);
    assert_eq!(report["decision"], "reject");
    assert!(report["reasons"].as_array().unwrap().iter().any(|r| r == "the candidate is the parent"));
    let (success, report, stderr) = shadow("useless.kvf", &["--fail-on-reject"]);
    assert!(!success);
    assert_eq!(report["decision"], "reject");
    assert!(stderr.contains("rejected"));

    // Held-out data with an event neither vocabulary has: carried by
    // escape, refused outright when asked to be strict.
    std::fs::write(dir.join("novel.jsonl"), format!("{}{{\"event\":\"BRAND_NEW\"}}\n", events(4, 2000, true))).unwrap();
    let (success, report, _) = run(&dir, &["shadow", "--parent", "parent.kvf", "--candidate", "cycle.kvf", "--input", "novel.jsonl", "--horizon", "100000"]);
    assert!(success);
    assert_eq!(report["parent"]["measured"]["escaped_symbols"], 1);
    assert_eq!(report["candidate"]["measured"]["escaped_symbols"], 1);
    let (success, _, stderr) = run(&dir, &["shadow", "--parent", "parent.kvf", "--candidate", "cycle.kvf", "--input", "novel.jsonl", "--unknown", "strict"]);
    assert!(!success);
    assert!(stderr.contains("the parent vocabulary cannot carry the inputs"), "{stderr}");

    // Terms that make no sense are refused.
    for bad in [["--horizon", "0"], ["--min-improvement", "1.5"], ["--max-slowdown", "0.5"], ["--passes", "0"]] {
        assert!(!shadow("cycle.kvf", &bad).0, "{bad:?}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
