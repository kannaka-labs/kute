//! `kute predict` as a process. Synthetic data.

use serde_json::Value;
use std::process::Command;

#[test]
fn predict_reports_what_the_past_adds_and_refuses_what_it_cannot_measure() {
    let dir = std::env::temp_dir().join(format!("kute-cli-predict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Each machine cycles A, B, C; one event in twenty is something else.
    let names = ["A", "B", "C"];
    let mut state = 9u64;
    let mut at = [0usize; 10];
    let mut lines = String::new();
    for t in 0..20_000u64 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let m = (state % 10) as usize;
        if (state >> 20) % 20 != 0 {
            at[m] = (at[m] + 1) % 3;
        } else {
            at[m] = ((state >> 30) % 3) as usize;
        }
        lines.push_str(&format!("{{\"machine\":{m},\"event\":\"{}\",\"t\":{t}}}\n", names[at[m]]));
    }
    std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(args).output().unwrap();

    let out = run(&["predict", "--input", "events.jsonl", "--symbol-field", "event", "--time-field", "t", "--group-field", "machine", "--output", "report.json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report, serde_json::from_slice::<Value>(&std::fs::read(dir.join("report.json")).unwrap()).unwrap());
    assert_eq!(report["held_out_events"], 5000);
    assert_eq!(report["predictors_compared"], 3);
    // Three events about equally common; the one before says nearly all.
    assert!(report["frequency"]["bits_per_event"].as_f64().unwrap() > 1.5);
    assert!(report["previous"]["bits_per_event"].as_f64().unwrap() < 0.5);
    assert!(report["previous"]["top1_accuracy"].as_f64().unwrap() > 0.9);
    assert!(report["previous_over_frequency"]["bits_per_event"].as_f64().unwrap() > 1.0);
    assert!(report["note"].as_str().unwrap().contains("nothing here shows that one causes the other"));

    // Without the entity field the machines are interleaved into one
    // record and the order within each is lost.
    let out = run(&["predict", "--input", "events.jsonl", "--symbol-field", "event", "--time-field", "t"]);
    assert!(out.status.success());
    let mixed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(mixed["previous"]["bits_per_event"].as_f64().unwrap() > 1.4, "{mixed}");

    for bad in [
        vec!["predict", "--input", "events.jsonl", "--symbol-field", "event", "--time-field", "t", "--holdout-fraction", "1"],
        vec!["predict", "--input", "events.jsonl", "--symbol-field", "event", "--time-field", "t", "--max-context", "9"],
        vec!["predict", "--input", "events.jsonl", "--symbol-field", "event", "--time-field", "missing"],
        vec!["predict", "--input", "events.jsonl", "--symbol-field", "event"],
    ] {
        assert!(!run(&bad).status.success(), "{bad:?}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
