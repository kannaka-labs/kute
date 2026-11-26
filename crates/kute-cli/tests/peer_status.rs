//! `kute peer serve --status-file` and `kute serve --peer-status`: a
//! running peer says what it is doing in a file, and the HTTP service
//! reads the file for `GET /v1/peer`. Both are real processes here.

use serde_json::Value;
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

fn prepare(dir: &Path) {
    let events: String = (0..400).map(|i| format!("{{\"event\":\"{}\"}}\n", ["OPEN", "READ", "CLOSE", "SYNC"][i % 4])).collect();
    std::fs::write(dir.join("log.jsonl"), events).unwrap();
    ok(dir, &["train", "--input", "log.jsonl", "--id", "test:peer-status:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "8", "--output", "events.kvf"]);
}

/// A `kute peer serve` with a status file, killed when dropped.
struct ServedPeer {
    child: Option<Child>,
    address: String,
}

impl Drop for ServedPeer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl ServedPeer {
    fn start(dir: &Path, status: &str, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute"))
            .current_dir(dir)
            .args(["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--accept-vocabularies", "--status-file", status])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut errors = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        errors.read_line(&mut line).unwrap();
        assert!(line.contains("listening"), "{line}");
        // The rest of what it says is read and dropped, so it never waits on a full pipe.
        std::thread::spawn(move || {
            let mut rest = String::new();
            let _ = errors.read_to_string(&mut rest);
        });
        Self { child: Some(child), address: line.trim().rsplit(' ').next().unwrap().to_string() }
    }

    /// Wait for it to end by itself; whether it succeeded.
    fn ended(&mut self) -> bool {
        self.child.take().unwrap().wait_with_output().unwrap().status.success()
    }
}

struct Service {
    child: Child,
    address: String,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    fn start(dir: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(["serve", "--listen", "127.0.0.1:0", "--vocab", "events.kvf"]).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut text = String::new();
        let said: Value = loop {
            let mut line = String::new();
            assert!(out.read_line(&mut line).unwrap() > 0, "the service ended before it said where it listens: {text}");
            text.push_str(&line);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                break value;
            }
        };
        Self { child, address: said["listening"].as_str().unwrap().to_string() }
    }

    fn request(&self, method: &str, path: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n").as_bytes()).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let at = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(answer[..at].to_vec()).unwrap();
        (head.split(' ').nth(1).unwrap().parse().unwrap(), serde_json::from_slice(&answer[at + 4..]).unwrap())
    }

    fn peer(&self) -> Value {
        let (status, body) = self.request("GET", "/v1/peer");
        assert_eq!(status, 200, "{body}");
        body
    }

    /// Ask until the answer satisfies `wanted`, for at most `seconds`.
    fn until(&self, seconds: u64, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        let started = Instant::now();
        loop {
            let answer = self.peer();
            if wanted(&answer) {
                return answer;
            }
            assert!(started.elapsed() < Duration::from_secs(seconds), "never saw {what}; last: {answer}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn a_peer_says_what_it_is_doing_from_waiting_to_ended() {
    let dir = scratch("peer-status-life");
    prepare(&dir);
    let service = Service::start(&dir, &["--peer-status", "peer.json"]);
    assert_eq!(service.request("GET", "/v1/capabilities").1["peer_status"], true);
    // Before any peer: the file is not there, and that is the answer.
    let nothing = service.peer();
    assert_eq!(nothing["present"], false);
    assert!(nothing["why"].as_str().unwrap().contains("no status file"), "{nothing}");

    let mut peer = ServedPeer::start(&dir, "peer.json", &["--timeout", "30"]);
    let waiting = service.until(5, "a waiting peer", |a| a["present"] == true);
    let status = &waiting["status"];
    assert_eq!(status["state"], "waiting for a connection");
    assert_eq!(status["listening"], peer.address.as_str());
    assert_eq!(status["kute_peer_status"], 1);
    assert_eq!(status["takes"]["vocabularies"], true);
    assert_eq!(status["takes"]["proposals"], false);
    assert_eq!(status["gives"]["vocabularies"], false);
    assert!(status["transport"].as_str().unwrap().starts_with("plain"));
    assert_eq!(status["exchange"], Value::Null);
    assert_eq!(waiting["stale"], false);
    assert!(status["software"].as_str().unwrap().starts_with("kute "));

    // It keeps writing while it waits: the file does not grow old.
    std::thread::sleep(Duration::from_secs(4));
    let later = service.peer();
    assert_eq!(later["stale"], false, "{later}");
    assert!(later["age_seconds"].as_u64().unwrap() <= 3, "{later}");
    assert!(later["status"]["written"].as_u64().unwrap() > status["written"].as_u64().unwrap());

    // Someone connects and says nothing yet: the peer is in an exchange with that address.
    let quiet = TcpStream::connect(&peer.address).unwrap();
    let mine = quiet.local_addr().unwrap().to_string();
    let busy = service.until(5, "an exchange", |a| a["status"]["state"] == "in an exchange");
    assert_eq!(busy["status"]["exchange"]["from"], mine.as_str());
    assert_eq!(busy["status"]["exchange"]["peer_identity"], Value::Null);
    assert_eq!(busy["status"]["exchange"]["messages_handled"], 0);
    assert_eq!(busy["stale"], false);
    // The connection closes having sent nothing: the peer ends, and says so.
    drop(quiet);
    let ended = service.until(5, "the end", |a| a["status"]["state"] == "ended");
    assert_eq!(ended["status"]["ended"]["ok"], true, "{ended}");
    assert_eq!(ended["status"]["exchange"]["messages_handled"], 0);
    assert!(peer.ended());
    // An ended peer is not called stale however old its file grows, and not called running.
    std::thread::sleep(Duration::from_secs(4));
    let after = service.peer();
    assert_eq!(after["stale"], false);
    assert!(after["means"].as_str().unwrap().contains("nothing is running"), "{after}");
    // Nothing was left beside the file.
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.contains("writing")).collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_real_exchange_a_peer_that_gives_up_and_a_peer_that_is_killed() {
    let dir = scratch("peer-status-ends");
    prepare(&dir);
    let service = Service::start(&dir, &["--peer-status", "peer.json"]);

    // A whole exchange: the status ends with what the peer's own report says.
    let mut peer = ServedPeer::start(&dir, "peer.json", &["--output-dir", "received"]);
    let sent = ok(&dir, &["peer", "send", "--connect", &peer.address, "--insecure-plain", "--vocab", "events.kvf", "--input", "log.jsonl"]);
    assert_eq!(sent["vocabulary_transferred"], true, "{sent}");
    let mut child = peer.child.take().unwrap();
    let mut printed = String::new();
    child.stdout.take().unwrap().read_to_string(&mut printed).unwrap();
    assert!(child.wait().unwrap().success());
    let report: Value = serde_json::from_str(&printed).unwrap();
    let ended = service.until(5, "the end", |a| a["status"]["state"] == "ended");
    let exchange = &ended["status"]["exchange"];
    assert_eq!(ended["status"]["ended"]["ok"], true);
    assert_eq!(exchange["messages_handled"], report["messages_handled"]);
    assert_eq!(exchange["delivered"], 1);
    assert_eq!(exchange["from"], report["from"]);
    assert_eq!(exchange["vocabularies_received"], report["vocabularies_received"]);
    assert_eq!(exchange["vocabularies_received"].as_array().unwrap().len(), 1);

    // A connection that says nothing for longer than the peer waits: it ends with an error, and says which.
    let mut peer = ServedPeer::start(&dir, "peer.json", &["--timeout", "1"]);
    let quiet = TcpStream::connect(&peer.address).unwrap();
    let ended = service.until(8, "an end in error", |a| a["status"]["state"] == "ended" && a["status"]["listening"] == peer.address.as_str());
    assert_eq!(ended["status"]["ended"]["ok"], false, "{ended}");
    assert!(ended["status"]["ended"]["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert!(!peer.ended());
    drop(quiet);

    // A peer that is killed says nothing. Its file still says "waiting";
    // the service reports it stale and does not call it running.
    let peer = ServedPeer::start(&dir, "peer.json", &[]);
    service.until(5, "the new peer", |a| a["status"]["state"] == "waiting for a connection" && a["status"]["listening"] == peer.address.as_str());
    drop(peer);
    let stale = service.until(10, "a stale file", |a| a["stale"] == true);
    assert_eq!(stale["status"]["state"], "waiting for a connection");
    assert!(stale["age_seconds"].as_u64().unwrap() > 3);
    assert!(stale["means"].as_str().unwrap().contains("Nothing here shows a running peer"), "{stale}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_service_reads_only_a_status_file_and_only_when_told_of_one() {
    let dir = scratch("peer-status-files");
    prepare(&dir);
    // Not told of a peer: the path does not exist.
    let plain = Service::start(&dir, &[]);
    assert_eq!(plain.request("GET", "/v1/peer").0, 404);
    assert_eq!(plain.request("GET", "/v1/capabilities").1["peer_status"], false);

    let service = Service::start(&dir, &["--peer-status", "status.json"]);
    // Read with GET and nothing else.
    for method in ["POST", "PUT", "DELETE"] {
        assert_eq!(service.request(method, "/v1/peer").0, 405, "{method}");
    }
    assert_eq!(service.request("GET", "/v1/peer/extra").0, 404);
    // Things that are not a status are said not to be one, and their content is not passed on.
    let big = format!("{{\"kute_peer_status\":1,\"state\":\"waiting for a connection\",\"pad\":\"{}\"}}", "x".repeat(70_000));
    for (content, why) in [
        ("not json at all", "could not be read"),
        ("{\"hello\": \"SECRET-IN-ANOTHER-FILE\"}", "could not be read"),
        ("{\"kute_peer_status\": 2, \"state\": \"waiting for a connection\"}", "could not be read"),
        ("{\"kute_peer_status\": 1}", "could not be read"),
        ("[1, 2, 3]", "could not be read"),
        (big.as_str(), "not a status file"),
    ] {
        std::fs::write(dir.join("status.json"), content).unwrap();
        let answer = service.peer();
        assert_eq!(answer["present"], false, "{answer}");
        assert!(answer["why"].as_str().unwrap().contains(why), "{answer}");
        assert!(!answer.to_string().contains("SECRET"), "{answer}");
        assert_eq!(answer["status"], Value::Null);
    }
    std::fs::remove_file(dir.join("status.json")).unwrap();
    std::fs::create_dir(dir.join("status.json")).unwrap();
    assert_eq!(service.peer()["present"], false);
    std::fs::remove_dir(dir.join("status.json")).unwrap();
    // A status that claims to be running but carries no time, or an old one, is stale.
    std::fs::write(dir.join("status.json"), "{\"kute_peer_status\": 1, \"state\": \"in an exchange\"}").unwrap();
    assert_eq!(service.peer()["stale"], true);
    std::fs::write(dir.join("status.json"), "{\"kute_peer_status\": 1, \"state\": \"waiting for a connection\", \"written\": 1000, \"heartbeat_seconds\": 4000000000}").unwrap();
    assert_eq!(service.peer()["stale"], true, "a file cannot excuse itself with a long beat");
    // Nor with a time of writing that has not come yet: an hour ahead, or the largest number there is.
    let hour_ahead = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600;
    for written in [hour_ahead, u64::MAX] {
        std::fs::write(dir.join("status.json"), format!("{{\"kute_peer_status\": 1, \"state\": \"in an exchange\", \"written\": {written}}}")).unwrap();
        let answer = service.peer();
        assert_eq!(answer["stale"], true, "{answer}");
        assert_eq!(answer["written_later_than_now"], true, "{answer}");
    }

    // Where the option is refused.
    for bad in [
        vec!["serve", "--listen", "127.0.0.1:0", "--vocab", "events.kvf", "--registry-only", "--peer-status", "status.json"],
        vec!["serve", "--listen", "0.0.0.0:0", "--insecure-remote", "--vocab", "events.kvf", "--peer-status", "status.json"],
        // A status file that cannot be written is told before the peer listens.
        vec!["peer", "serve", "--listen", "127.0.0.1:0", "--insecure-plain", "--status-file", "no-such-folder/status.json"],
    ] {
        let out = Command::new("timeout").arg("20").arg(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(&bad).output().unwrap_or_else(|_| kute(&dir, &bad));
        assert!(!out.status.success(), "{bad:?} was accepted");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
