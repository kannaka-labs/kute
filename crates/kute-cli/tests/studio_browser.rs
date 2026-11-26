//! KUTE Studio in a real browser. A `kute serve --studio` process is
//! started, report files are made by the command line, and
//! `studio_browser.mjs` drives the page in headless Chrome or Edge over
//! the DevTools protocol: every view, the timing table, a running peer,
//! the reports, and what the browser's own policy refuses.
//!
//! It needs Node 22 or later and a Chromium browser, which a build
//! machine may not have, so it runs only when asked:
//!
//!   KUTE_STUDIO_BROWSER=require cargo test -p kute-cli --test studio_browser
//!
//! With `require`, a missing Node or browser is a failure. Unset, the
//! test says it was skipped and passes. `KUTE_STUDIO_BROWSER_PATH` names
//! the browser when it is not in a usual place, and
//! `KUTE_STUDIO_BROWSER_NO_SANDBOX=1` passes `--no-sandbox` for machines
//! where the browser's sandbox cannot start.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the command line; return what it printed, whatever it decided.
fn kute(dir: &Path, args: &[&str]) -> (bool, Vec<u8>) {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap();
    (out.status.success(), out.stdout)
}

fn ok(dir: &Path, args: &[&str]) -> Vec<u8> {
    let (success, stdout) = kute(dir, args);
    assert!(success, "kute {args:?} failed");
    stdout
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn browser() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("KUTE_STUDIO_BROWSER_PATH") {
        return Some(PathBuf::from(named));
    }
    let fixed = [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/microsoft-edge",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
        "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    ];
    fixed.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Node, if it is there and new enough to have `fetch` and `WebSocket`.
fn node() -> Result<(), String> {
    let out = Command::new("node").arg("--version").output().map_err(|e| format!("node was not found: {e}"))?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let major: u32 = version.trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major < 22 { Err(format!("node {version} is older than 22")) } else { Ok(()) }
}

#[test]
fn studio_works_in_a_browser() {
    let required = match std::env::var("KUTE_STUDIO_BROWSER").as_deref() {
        Ok("require") => true,
        Ok(other) => panic!("KUTE_STUDIO_BROWSER is {other:?}; the only value is \"require\""),
        Err(_) => {
            eprintln!("skipped: set KUTE_STUDIO_BROWSER=require to run Studio in a browser");
            return;
        }
    };
    assert!(required);
    node().unwrap();
    let browser = browser().expect("no Chromium browser was found; name one with KUTE_STUDIO_BROWSER_PATH");

    // Two vocabularies to serve, and a child of one for the shadow report.
    let dir = scratch("studio-browser");
    let cycle = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];
    let log: String = cycle.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect();
    std::fs::write(dir.join("log.jsonl"), log.repeat(40)).unwrap();
    std::fs::write(dir.join("text.txt"), "The engine reads the event log. The engine writes the event log.\n".repeat(30)).unwrap();
    ok(&dir, &["train", "--input", "log.jsonl", "--id", "test:studio:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "24", "--output", "events.kvf"]);
    ok(&dir, &["train", "--input", "text.txt", "--id", "test:studio:text", "--vocab-size", "300", "--output", "text.kvf"]);
    ok(&dir, &["evolve", "--vocab", "events.kvf", "--add", "VALVE_STUCK", "--new-version", "0.2.0", "--output", "child.kvf"]);

    // Reports, as the command line prints them. A shadow report is
    // printed whichever way the decision goes.
    let (_, shadow) = kute(&dir, &["shadow", "--parent", "events.kvf", "--candidate", "child.kvf", "--input", "log.jsonl"]);
    assert!(serde_json::from_slice::<Value>(&shadow).is_ok_and(|v| v["decision"].is_string()), "{}", String::from_utf8_lossy(&shadow));
    std::fs::write(dir.join("shadow.json"), &shadow).unwrap();
    // One exchange between two peers: what each side printed.
    let mut server = Command::new(env!("CARGO_BIN_EXE_kute"))
        .current_dir(&dir)
        .args(["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--output-dir", "received", "--timeout", "20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut errors = BufReader::new(server.stderr.take().unwrap());
    let mut line = String::new();
    errors.read_line(&mut line).unwrap();
    assert!(line.contains("listening"), "{line}");
    let address = line.trim().rsplit(' ').next().unwrap().to_string();
    let send = ok(&dir, &["peer", "send", "--connect", &address, "--insecure-plain", "--vocab", "events.kvf", "--input", "log.jsonl"]);
    let served = server.wait_with_output().unwrap();
    drop(errors);
    assert!(served.status.success());
    std::fs::write(dir.join("serve.json"), &served.stdout).unwrap();
    std::fs::write(dir.join("send.json"), &send).unwrap();
    // Appended one after the other, as `>> log` leaves them.
    std::fs::write(dir.join("both.log"), [served.stdout.as_slice(), send.as_slice()].concat()).unwrap();
    std::fs::write(dir.join("other.json"), b"{\"hello\": [1, 2, 3]}").unwrap();
    std::fs::write(dir.join("broken.json"), b"{\"hello\": [1, 2").unwrap();

    // A peer that is running while the page is looked at.
    let mut waiting = Command::new(env!("CARGO_BIN_EXE_kute"))
        .current_dir(&dir)
        .args(["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--status-file", "peer.json", "--timeout", "120"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(waiting.stderr.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("listening"), "{line}");
    let waiting = Killed(waiting);

    // The service with the page.
    let mut service = Killed(
        Command::new(env!("CARGO_BIN_EXE_kute"))
            .current_dir(&dir)
            .args(["serve", "--listen", "127.0.0.1:0", "--vocab", "events.kvf", "--vocab", "text.kvf", "--studio", "--peer-status", "peer.json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut out = BufReader::new(service.0.stdout.take().unwrap());
    let mut text = String::new();
    let studio = loop {
        let mut line = String::new();
        assert!(out.read_line(&mut line).unwrap() > 0, "the service ended before it said where it listens: {text}");
        text.push_str(&line);
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            break value["studio"].as_str().unwrap().to_string();
        }
    };

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("studio_browser.mjs");
    let run = Command::new("node").arg(&script).arg(&studio).arg(&browser).arg(&dir).output().unwrap();
    let said = format!("{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert!(run.status.success(), "Studio in {}:\n{said}", browser.display());
    let report: Value = serde_json::from_slice(&run.stdout).unwrap();
    // The script ran to its end: the last check is the one about script errors.
    assert!(report["checked"].as_array().unwrap().last().unwrap().as_str().unwrap().starts_with("no script error"), "{said}");
    println!("{said}");
    drop(service);
    drop(waiting);
    let _ = std::fs::remove_dir_all(&dir);
}
