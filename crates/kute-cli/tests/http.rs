//! Work package E1 of the v0.7 specification, the REST part: "versioned
//! encode, decode, discover, inspect, compare, capabilities; bounded
//! payloads." A real `kute serve` process is driven over a socket with
//! requests written by hand, so that nothing here trusts an HTTP library
//! to be polite.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kute(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap();
    assert!(out.status.success(), "kute {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute"))
            .current_dir(dir)
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The address is the first thing it prints, as one JSON value.
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut text = String::new();
        let address = loop {
            let mut line = String::new();
            assert!(out.read_line(&mut line).unwrap() > 0, "the service ended before it said where it listens: {text}");
            text.push_str(&line);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                assert_eq!(value["api"], "v1");
                assert_eq!(value["loopback_only"], true);
                break value["listening"].as_str().unwrap().to_string();
            }
        };
        Self { child, address }
    }

    /// Send raw bytes, read until the service closes, and split the answer.
    fn raw(&self, request: &[u8]) -> (u16, String, Value) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        stream.write_all(request).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let text = String::from_utf8(answer).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no header block in {text:?}"));
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        assert!(head.contains("Connection: close"), "{head}");
        assert!(head.contains("Content-Type: application/json"), "{head}");
        (status, head.to_string(), serde_json::from_str(body).unwrap_or_else(|e| panic!("body is not JSON ({e}): {body:?}")))
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let (status, _, body) = self.raw(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
        (status, body)
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let body = body.to_string();
        let (status, _, answer) =
            self.raw(format!("POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", self.address, body.len()).as_bytes());
        (status, answer)
    }

    fn ok(&self, operation: &str, body: Value) -> Value {
        let (status, answer) = self.post(&format!("/v1/{operation}"), &body);
        assert_eq!(status, 200, "{operation}: {answer}");
        answer
    }

    fn refused(&self, operation: &str, body: Value) -> String {
        let (status, answer) = self.post(&format!("/v1/{operation}"), &body);
        assert_eq!(status, 422, "{operation}: {answer}");
        assert_eq!(answer["error"]["code"], "refused");
        answer["error"]["message"].as_str().unwrap().to_string()
    }
}

fn vocabularies(dir: &Path) {
    let cycle = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];
    let log: String = cycle.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect();
    std::fs::write(dir.join("log.jsonl"), log.repeat(40)).unwrap();
    std::fs::write(dir.join("text.txt"), "The engine reads the event log. The engine writes the event log.\n".repeat(30)).unwrap();
    kute(dir, &["train", "--input", "log.jsonl", "--id", "test:http:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "24", "--output", "events.kvf"]);
    kute(dir, &["train", "--input", "text.txt", "--id", "test:http:text", "--vocab-size", "300", "--output", "text.kvf"]);
}

#[test]
fn the_operations_work_over_http_and_agree_with_the_command_line() {
    let dir = scratch("http");
    vocabularies(&dir);
    let service = Service::start(&dir, &["--vocab", "events.kvf", "--vocab", "text.kvf", "--max-input-bytes", "4096", "--max-output-bytes", "8192"]);

    // capabilities: what is held, what may be called, what it may cost.
    let (status, capabilities) = service.get("/v1/capabilities");
    assert_eq!(status, 200, "{capabilities}");
    assert_eq!(capabilities["api"], "v1");
    assert_eq!(capabilities["operations"], json!(["encode", "decode", "inspect", "compare", "discover"]), "nothing trains or publishes");
    assert_eq!(capabilities["vocabularies"].as_array().unwrap().len(), 2);
    assert_eq!(capabilities["limits"]["max_input_bytes"], 4096);
    assert!(capabilities["limits"]["max_body_bytes"].as_u64().unwrap() > 4096);

    // encode and decode, and the same ids as `kute encode` gives.
    let text = "The engine reads the log. Naïve café 🙂";
    let encoded = service.ok("encode", json!({ "vocabulary": "test:http:text", "text": text }));
    let decoded = service.ok("decode", json!({ "container_base64": encoded["container_base64"] }));
    assert_eq!(decoded["text"], text);
    std::fs::write(dir.join("one.txt"), text).unwrap();
    kute(&dir, &["encode", "--vocab", "text.kvf", "--input", "one.txt", "--output", "one.kute"]);
    let container = base64(&std::fs::read(dir.join("one.kute")).unwrap());
    let from_file = service.ok("decode", json!({ "container_base64": container }));
    assert_eq!(from_file["text"], text, "a container written by the command line decodes over HTTP");
    assert_eq!(from_file["tokens"], encoded["tokens"], "and holds as many tokens as the service produced");

    // inspect, compare, discover.
    let summary = service.ok("inspect", json!({ "vocabulary": "test:http:events" }));
    assert_eq!(summary["summary"]["algorithm"], "sequence-bpe");
    assert!(service.ok("compare", json!({ "a": "test:http:events", "b": "test:http:text" })).is_object());
    let records: Vec<Vec<&str>> = (0..30).map(|_| vec!["A", "B", "C", "A", "B", "C"]).collect();
    let found = service.ok("discover", json!({ "records": records }));
    assert!(found["patterns"].as_array().unwrap().iter().any(|p| p["symbols"] == json!(["A", "B", "C"])), "{found}");

    // What an operation refuses is a 422 with its reason.
    assert!(service.refused("encode", json!({ "vocabulary": "no:such", "text": "x" })).contains("no:such"));
    assert!(service.refused("encode", json!({ "vocabulary": "test:http:text", "text": "x".repeat(5000) })).contains("4096"));
    assert!(!service.refused("encode", json!([1, 2])).is_empty());
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn what_is_not_a_proper_request_is_refused_within_bounds() {
    let dir = scratch("http-bounds");
    vocabularies(&dir);
    let service = Service::start(&dir, &["--vocab", "text.kvf", "--max-input-bytes", "1024", "--timeout", "2", "--workers", "2"]);
    let code = |answer: &(u16, String, Value)| answer.2["error"]["code"].as_str().unwrap_or("").to_string();
    let host = "Host: localhost\r\n";

    // Paths and methods.
    assert_eq!(service.get("/").0, 404);
    assert_eq!(service.get("/v2/capabilities").0, 404);
    assert_eq!(service.get("/v1/train").0, 404, "training is not offered");
    let wrong = service.raw(format!("GET /v1/encode HTTP/1.1\r\n{host}\r\n").as_bytes());
    assert_eq!((wrong.0, code(&wrong).as_str()), (405, "method_not_allowed"));
    assert!(wrong.1.contains("Allow: POST"));
    assert_eq!(service.raw(format!("POST /v1/capabilities HTTP/1.1\r\n{host}Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}").as_bytes()).0, 405);

    // A web page cannot be made to call it: the host must be a loopback
    // name, and the body must be declared as JSON.
    let other = service.raw(b"GET /v1/capabilities HTTP/1.1\r\nHost: evil.example\r\n\r\n");
    assert_eq!((other.0, code(&other).as_str()), (403, "forbidden_host"));
    assert_eq!(service.raw(b"GET /v1/capabilities HTTP/1.1\r\n\r\n").0, 403, "no Host at all");
    let form = service.raw(format!("POST /v1/inspect HTTP/1.1\r\n{host}Content-Type: text/plain\r\nContent-Length: 2\r\n\r\n{{}}").as_bytes());
    assert_eq!((form.0, code(&form).as_str()), (415, "unsupported_media_type"));

    // Bodies: a length is required, is believed before anything is read, and must be JSON.
    let json_head = format!("POST /v1/inspect HTTP/1.1\r\n{host}Content-Type: application/json\r\n");
    assert_eq!(service.raw(format!("{json_head}\r\n").as_bytes()).0, 411);
    assert_eq!(service.raw(format!("{json_head}Transfer-Encoding: chunked\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n").as_bytes()).0, 411);
    let started = Instant::now();
    let huge = service.raw(format!("{json_head}Content-Length: 999999999999\r\n\r\n").as_bytes());
    assert_eq!((huge.0, code(&huge).as_str()), (413, "too_large"));
    assert!(started.elapsed() < Duration::from_secs(1), "refused on the declared length, without waiting for the body");
    let broken = service.raw(format!("{json_head}Content-Length: 5\r\n\r\n{{\"a\":").as_bytes());
    assert_eq!((broken.0, code(&broken).as_str()), (400, "bad_json"));
    assert_eq!(service.raw(format!("{json_head}Content-Length: 2\r\n\r\n{{}}extra").as_bytes()).0, 400);
    assert_eq!(service.raw(format!("{json_head}Content-Length: 2\r\nContent-Length: 3\r\n\r\n{{}}").as_bytes()).0, 400);

    // Headers: not HTTP, and too many of them.
    assert_eq!(service.raw(b"hello\r\n\r\n").0, 400);
    let mut long = format!("GET /v1/capabilities HTTP/1.1\r\n{host}").into_bytes();
    long.extend(format!("X-Filler: {}\r\n", "a".repeat(1000)).repeat(20).into_bytes());
    long.extend(b"\r\n");
    assert_eq!(service.raw(&long).0, 431);

    // A request that stops arriving is given up on after the timeout.
    let started = Instant::now();
    let slow = service.raw(format!("{json_head}Content-Length: 50\r\n\r\n{{").as_bytes());
    assert_eq!((slow.0, code(&slow).as_str()), (408, "timeout"));
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1500) && waited < Duration::from_secs(8), "{waited:?}");

    // Idle connections that hold every worker and fill the queue do not
    // stop the service from answering: the next caller is told it is busy,
    // and once they time out it serves again.
    let idle: Vec<TcpStream> = (0..2 + 16).map(|_| TcpStream::connect(&service.address).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(300));
    let turned_away = service.raw(format!("GET /v1/capabilities HTTP/1.1\r\n{host}\r\n").as_bytes());
    assert_eq!((turned_away.0, code(&turned_away).as_str()), (503, "busy"));
    drop(idle);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(service.get("/v1/capabilities").0, 200);
}

#[test]
fn an_address_that_is_not_loopback_has_to_be_asked_for() {
    let dir = scratch("http-remote");
    vocabularies(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(["serve", "--vocab", "text.kvf", "--listen", "0.0.0.0:0"]).output().unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("--insecure-remote") && said.contains("no authentication"), "{said}");
}
