//! `kute live` as a process: windows out, a checkpoint, and going on from it.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn kute() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kute"))
}

/// Six hours of events from 20 machines, one a second, slightly out of
/// order, as JSON Lines.
fn events() -> String {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let cycle = ["START", "HIGH", "TRIP", "ACK"];
    let mut step = [0usize; 20];
    let mut rows: Vec<(i64, String)> = Vec::new();
    for t in 0..21_600i64 {
        let machine = next(20) as usize;
        let name = if next(3) == 0 {
            step[machine] += 1;
            cycle[step[machine] % 4]
        } else {
            ["IDLE", "TICK", "FAN"][next(3) as usize]
        };
        // The machine is a number for some and a string for others.
        let id = if machine % 2 == 0 { format!("{machine}") } else { format!("\"m{machine}\"") };
        rows.push((t + next(4) as i64, format!("{{\"machine\":{id},\"name\":\"{name}\",\"at\":{t},\"extra\":[1,2]}}\n")));
    }
    rows.sort_by_key(|r| r.0);
    rows.into_iter().map(|r| r.1).collect()
}

fn lines(output: &[u8]) -> (Vec<Value>, Value) {
    let mut windows = Vec::new();
    let mut summary = Value::Null;
    for line in std::str::from_utf8(output).unwrap().lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        if value.get("window").is_some() {
            let mut window = value["window"].clone();
            // The bytes of state depend on what is buffered at the moment.
            window["state_bytes"] = Value::Null;
            windows.push(window);
        } else {
            summary = value["summary"].clone();
        }
    }
    (windows, summary)
}

const ARGS: [&str; 12] = ["--symbol-field", "name", "--time-field", "at", "--group-field", "machine", "--pane", "3600", "--panes", "2", "--lateness", "10"];

#[test]
fn windows_come_out_and_a_run_resumed_from_its_checkpoint_gives_the_same() {
    let dir = std::env::temp_dir().join(format!("kute-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let text = events();
    let whole = dir.join("events.jsonl");
    std::fs::write(&whole, &text).unwrap();

    // From a file, and the same from a pipe.
    let from_file = kute().arg("live").arg(&whole).args(ARGS).output().unwrap();
    assert!(from_file.status.success(), "{}", String::from_utf8_lossy(&from_file.stderr));
    let (windows, summary) = lines(&from_file.stdout);
    assert_eq!(windows.len(), 6);
    assert_eq!(summary["totals"]["counted"], 21_600);
    assert_eq!(summary["totals"]["late_dropped"], 0);
    for (hour, window) in windows.iter().enumerate() {
        assert_eq!(window["end"], (hour as i64 + 1) * 3600);
        assert_eq!(window["exact"], true);
        let found = window["patterns"].as_array().unwrap();
        assert!(found.iter().any(|p| p["symbols"] == serde_json::json!(["START", "HIGH", "TRIP", "ACK"])), "hour {hour}");
    }
    let mut piped = kute().arg("live").args(ARGS).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = piped.stdin.take().unwrap();
    let copy = text.clone();
    let writer = std::thread::spawn(move || stdin.write_all(copy.as_bytes()).unwrap());
    let from_pipe = piped.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(lines(&from_pipe.stdout).0, windows);

    // A run that stops partway: the input ends in the fourth hour, and
    // its checkpoint is from the last window that closed before that.
    let cut = text.match_indices('\n').nth(12_500).unwrap().0 + 1;
    let part = dir.join("part.jsonl");
    std::fs::write(&part, &text[..cut]).unwrap();
    let checkpoint = dir.join("state.json");
    let first = kute().arg("live").arg(&part).args(ARGS).arg("--checkpoint").arg(&checkpoint).output().unwrap();
    assert!(first.status.success());
    let (before, first_summary) = lines(&first.stdout);
    assert_eq!(first_summary["checkpoints_written"], 3);
    // Going on from the checkpoint over the whole input: the windows
    // after it are those of the uninterrupted run.
    let second = kute().arg("live").arg(&whole).args(ARGS).arg("--checkpoint").arg(&checkpoint).arg("--resume").output().unwrap();
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let (after, second_summary) = lines(&second.stdout);
    assert!(second_summary["lines_skipped_on_resume"].as_u64().unwrap() > 10_000);
    assert_eq!(before[..3], windows[..3]);
    assert_eq!(after, windows[3..]);
    assert_eq!(second_summary["totals"], summary["totals"]);

    // Other parameters than the checkpoint's, or a shorter input, are refused.
    let other = kute().arg("live").arg(&whole).args(&ARGS[..8]).arg("--checkpoint").arg(&checkpoint).arg("--resume").output().unwrap();
    assert!(!other.status.success());
    let checkpoint_now = std::fs::read(&checkpoint).unwrap();
    let short = dir.join("short.jsonl");
    std::fs::write(&short, &text[..1000]).unwrap();
    let shorter = kute().arg("live").arg(&short).args(ARGS).arg("--checkpoint").arg(&checkpoint).arg("--resume").output().unwrap();
    assert!(!shorter.status.success());
    assert_eq!(std::fs::read(&checkpoint).unwrap(), checkpoint_now);

    // A line that is not an event stops the run with its number, or is
    // counted and passed over when asked.
    let dirty = dir.join("dirty.jsonl");
    std::fs::write(&dirty, format!("{}not json\n{{\"name\":\"X\",\"machine\":1}}\n{}", &text[..cut], &text[cut..])).unwrap();
    let stopped = kute().arg("live").arg(&dirty).args(ARGS).output().unwrap();
    assert!(!stopped.status.success());
    assert!(String::from_utf8_lossy(&stopped.stderr).contains("line 12502"), "{}", String::from_utf8_lossy(&stopped.stderr));
    let passed = kute().arg("live").arg(&dirty).args(ARGS).arg("--skip-bad-lines").output().unwrap();
    let (dirty_windows, dirty_summary) = lines(&passed.stdout);
    assert_eq!(dirty_summary["bad_lines"], 2);
    assert_eq!(dirty_windows, windows);

    std::fs::remove_dir_all(&dir).unwrap();
}
