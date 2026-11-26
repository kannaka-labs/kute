//! Work package E1 of the v0.7 specification, the last REST item:
//! "train/jobs", which it allows only with "explicit permission and
//! quotas". A real `kute serve --jobs-dir` is driven over a socket.

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

struct Service {
    child: Child,
    address: String,
    said: Value,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    fn start(dir: &Path, args: &[&str]) -> Self {
        let mut child =
            Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(["serve", "--listen", "127.0.0.1:0"]).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut text = String::new();
        let said = loop {
            let mut line = String::new();
            assert!(out.read_line(&mut line).unwrap() > 0, "the service ended before it said where it listens: {text}");
            text.push_str(&line);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                break value;
            }
        };
        Self { child, address: said["listening"].as_str().unwrap().to_string(), said }
    }

    fn raw(&self, request: &[u8]) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        stream.write_all(request).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let at = answer.windows(4).position(|w| w == b"\r\n\r\n").expect("a header block");
        let head = String::from_utf8(answer[..at].to_vec()).unwrap();
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head, answer[at + 4..].to_vec())
    }

    fn call(&self, method: &str, path: &str) -> (u16, String, Value) {
        let (status, head, body) = self.raw(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
        (status, head, serde_json::from_slice(&body).unwrap())
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let (status, _, body) = self.call("GET", path);
        (status, body)
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let body = body.to_string();
        let (status, _, answer) = self.raw(format!("POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", self.address, body.len()).as_bytes());
        (status, serde_json::from_slice(&answer).unwrap())
    }

    /// Wait until a job has finished, one way or the other.
    fn finished(&self, id: &str) -> Value {
        let started = Instant::now();
        loop {
            let (status, job) = self.get(&format!("/v1/jobs/{id}"));
            assert_eq!(status, 200, "{job}");
            if job["finished_unix"].is_u64() {
                return job;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "the job did not finish: {job}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

const TEXT: &str = "The engine reads the event log. The engine writes the event log.\n";

#[test]
fn a_job_trains_what_the_command_line_trains_and_leaves_a_file_named_by_its_digest() {
    let dir = scratch("jobs");
    let corpus = TEXT.repeat(30);
    std::fs::write(dir.join("text.txt"), &corpus).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).args(["train", "--input", "text.txt", "--id", "test:jobs:text", "--vocab-size", "300", "--output", "by-hand.kvf"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let by_hand: Value = serde_json::from_slice(&out.stdout).unwrap();
    let digest = by_hand["summary"]["digest"].as_str().expect("the summary names the digest").to_string();

    let service = Service::start(&dir, &["--jobs-dir", "made"]);
    assert_eq!(service.said["jobs"], true);
    let (_, capabilities) = service.get("/v1/capabilities");
    assert_eq!(capabilities["jobs"]["running_at_once"], 1);
    assert_eq!(capabilities["jobs"]["max_vocab_size"], 65_536);
    assert_eq!(capabilities["operations"], json!(["encode", "decode", "inspect", "compare", "discover"]), "a job is not one of the operations");

    let (status, job) = service.post("/v1/jobs", &json!({ "texts": [corpus], "vocabulary_id": "test:jobs:text", "vocab_size": 300 }));
    assert_eq!(status, 202, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["asked"]["corpus_bytes"], corpus.len());
    assert!(job["asked"].get("texts").is_none(), "the corpus is not said back");
    // Before it is done there is no vocabulary to give, and that is not "no such job".
    let early = service.get(&format!("/v1/jobs/{id}/vocabulary"));
    assert!(early.0 == 409 || early.0 == 200, "{early:?}");

    let done = service.finished(&id);
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["result"]["digest"], digest, "the same corpus and settings give the vocabulary `kute train` gives");
    let name = format!("{}.kvf", digest.strip_prefix("sha256:").unwrap());
    assert_eq!(done["result"]["file"], name);
    let on_disk = std::fs::read(dir.join("made").join(&name)).unwrap();
    let (status, _, given) = service.raw(format!("GET /v1/jobs/{id}/vocabulary HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
    assert_eq!(status, 200);
    assert_eq!(given, on_disk);
    // The file is a vocabulary the command line reads, with that digest.
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).arg("inspect").arg(dir.join("made").join(&name)).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["summary"]["digest"], digest);
    // Nothing else was left in the directory.
    let left: Vec<String> = std::fs::read_dir(dir.join("made")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, vec![name]);

    // The service does not start using what it trained.
    let (_, capabilities) = service.get("/v1/capabilities");
    assert_eq!(capabilities["vocabularies"].as_array().unwrap().len(), 0);
    let (status, _) = service.post("/v1/encode", &json!({ "vocabulary": digest, "text": "The engine" }));
    assert_eq!(status, 422);

    // The list has it, and a job that fails says why and has no file.
    let (status, bad) = service.post("/v1/jobs", &json!({ "texts": ["x"], "vocabulary_id": "test:jobs:none", "vocab_size": 300, "adapter": "jsonl-events" }));
    assert_eq!(status, 202, "{bad}");
    let failed = service.finished(bad["id"].as_str().unwrap());
    assert_eq!(failed["state"], "failed");
    assert!(!failed["error"].as_str().unwrap().is_empty(), "{failed}");
    let (status, why) = service.get(&format!("/v1/jobs/{}/vocabulary", bad["id"].as_str().unwrap()));
    assert_eq!((status, why["error"]["code"].as_str()), (409, Some("not_ready")));
    let (_, list) = service.get("/v1/jobs");
    assert_eq!(list["jobs"].as_array().unwrap().len(), 2);
}

#[test]
fn what_a_job_may_ask_is_bounded_and_nothing_else_can_be_done_to_one() {
    let dir = scratch("jobs-bounds");
    let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-vocab-size", "400", "--jobs-queue", "1"]);
    let refused = |body: Value| {
        let (status, answer) = service.post("/v1/jobs", &body);
        assert_eq!(status, 422, "{answer}");
        answer["error"]["message"].as_str().unwrap().to_string()
    };
    assert!(refused(json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 401 })).contains("more than this service trains"));
    assert!(refused(json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 300, "output": "/tmp/x" })).contains("unknown field"));
    assert!(refused(json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 300, "input": "C:/secret.txt" })).contains("unknown field"), "a caller cannot name a file to read");
    assert!(refused(json!({ "vocabulary_id": "test:a", "vocab_size": 300 })).contains("texts is required"));
    assert!(refused(json!({ "texts": [], "vocabulary_id": "test:a", "vocab_size": 300 })).contains("empty"));
    assert!(refused(json!({ "texts": [1], "vocabulary_id": "test:a", "vocab_size": 300 })).contains("not a string"));
    assert!(refused(json!({ "texts": ["x"], "vocab_size": 300 })).contains("vocabulary_id is required"));
    assert!(refused(json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 300, "operation": "evolve" })).contains("only job is train"));
    assert!(refused(json!(["x"])).contains("JSON object"));
    // What training would refuse for certain takes no place in the line.
    let with = |name: &str, value: Value| {
        let mut body = json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 300 });
        body[name] = value;
        body
    };
    assert!(refused(with("algorithm", json!("no-such-algorithm"))).contains("unknown algorithm"));
    assert!(refused(with("algorithm", json!("dictionary"))).contains("needs entries"));
    assert!(refused(with("adapter", json!("no-such-adapter"))).contains("unknown adapter"));
    assert!(refused(with("vocab_size", json!(0))).contains("vocab_size"));
    assert!(refused(with("vocab_size", json!(1))).contains("256"));
    assert!(refused(with("min_frequency", json!(0))).contains("min_frequency"));
    let (_, list) = service.get("/v1/jobs");
    assert_eq!(list["jobs"].as_array().unwrap().len(), 0, "a refused job is not a job");

    // No job by that name, whatever the name looks like.
    for path in ["/v1/jobs/job-99", "/v1/jobs/..%2F..%2Fetc", "/v1/jobs/job-1/vocabulary/more", "/v1/jobs/job-1/corpus"] {
        assert_eq!(service.get(path).0, 404, "{path}");
    }
    // A job cannot be changed or stopped.
    for method in ["DELETE", "PUT", "PATCH"] {
        let (status, head, _) = service.call(method, "/v1/jobs/job-1");
        assert_eq!(status, 405, "{method}");
        assert!(head.contains("Allow: GET"), "{head}");
        assert_eq!(service.call(method, "/v1/jobs").0, 405);
    }
    // A form post from another page is not JSON and is not read.
    let body = "texts=x";
    let (status, _, _) = service.raw(format!("POST /v1/jobs HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    assert_eq!(status, 415);

    // One runs, one waits, the rest are told to come back; every job
    // that was taken finishes.
    let corpus: String = (0..40_000).map(|i| format!("word{} and{} the{} ", i % 977, i % 31, i % 7)).collect();
    let mut taken = Vec::new();
    let mut turned_away = 0;
    for n in 0..8 {
        let (status, answer) = service.post("/v1/jobs", &json!({ "texts": [corpus], "vocabulary_id": format!("test:burst{n}"), "vocab_size": 400 }));
        match status {
            202 => taken.push(answer["id"].as_str().unwrap().to_string()),
            503 => {
                assert_eq!(answer["error"]["code"], "busy");
                turned_away += 1;
            }
            other => panic!("{other}: {answer}"),
        }
    }
    assert!(turned_away > 0, "eight jobs at once were all taken with room for one waiting");
    assert!(!taken.is_empty());
    for id in &taken {
        assert_eq!(service.finished(id)["state"], "done");
    }
}

/// A vocabulary larger than the whole limit fits no directory. It fails,
/// and the jobs after it that do fit are still taken: one job cannot
/// close the door on a directory with room in it.
#[test]
fn a_vocabulary_larger_than_the_whole_limit_does_not_stop_the_jobs_after_it() {
    let dir = scratch("jobs-too-large");
    let job = |id: &str, vocab_size: u32| json!({ "texts": [TEXT.repeat(30)], "vocabulary_id": id, "vocab_size": vocab_size });
    let (large, small) = {
        let service = Service::start(&dir, &["--jobs-dir", "sizes"]);
        let size = |id: &str, vocab_size: u32| {
            let (status, taken) = service.post("/v1/jobs", &job(id, vocab_size));
            assert_eq!(status, 202, "{taken}");
            service.finished(taken["id"].as_str().unwrap())["result"]["file_bytes"].as_u64().unwrap()
        };
        (size("test:large", 300), size("test:small", 256))
    };
    assert!(small < large, "{small} {large}");
    let limit = (large - 1).to_string();
    let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-dir-bytes", &limit]);
    let (status, taken) = service.post("/v1/jobs", &job("test:large", 300));
    assert_eq!(status, 202, "{taken}");
    let failed = service.finished(taken["id"].as_str().unwrap());
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(service.get("/v1/jobs").1["directory"]["bytes"], 0);
    let (status, taken) = service.post("/v1/jobs", &job("test:small", 256));
    assert_eq!(status, 202, "the directory is empty and this one fits: {taken}");
    assert_eq!(service.finished(taken["id"].as_str().unwrap())["state"], "done");
}

#[test]
fn the_directory_holds_no_more_than_its_operator_allowed() {
    let dir = scratch("jobs-room");
    let job = |id: &str| json!({ "texts": [TEXT.repeat(30)], "vocabulary_id": id, "vocab_size": 300 });
    let size = {
        let service = Service::start(&dir, &["--jobs-dir", "made"]);
        let (_, list) = service.get("/v1/jobs");
        assert_eq!(list["directory"], json!({ "files": 0, "bytes": 0, "max_bytes": 1u64 << 30 }));
        let (status, taken) = service.post("/v1/jobs", &job("test:room:a"));
        assert_eq!(status, 202, "{taken}");
        let done = service.finished(taken["id"].as_str().unwrap());
        let size = done["result"]["file_bytes"].as_u64().unwrap();
        let (_, list) = service.get("/v1/jobs");
        assert_eq!(list["directory"]["files"], 1);
        assert_eq!(list["directory"]["bytes"], size);
        size
    };
    // Full: nothing is taken, and the answer says how full.
    {
        let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-dir-bytes", &size.to_string()]);
        let (status, answer) = service.post("/v1/jobs", &job("test:room:b"));
        assert_eq!((status, answer["error"]["code"].as_str()), (507, Some("no_room")), "{answer}");
        assert!(answer["error"]["message"].as_str().unwrap().contains(&size.to_string()), "{answer}");
        assert_eq!(service.get("/v1/jobs").1["jobs"].as_array().unwrap().len(), 0);
    }
    // Room for a job to be taken but not for its file: it fails and
    // writes nothing. The vocabulary already there is given again.
    let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-dir-bytes", &(size + 10).to_string()]);
    let (status, taken) = service.post("/v1/jobs", &job("test:room:b"));
    assert_eq!(status, 202, "{taken}");
    let failed = service.finished(taken["id"].as_str().unwrap());
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(failed["error"].as_str().unwrap().contains("may hold"), "{failed}");
    // Having seen one not fit, the service takes no more while that
    // little room is left, so nothing trains only to fail.
    let (status, answer) = service.post("/v1/jobs", &job("test:room:a"));
    assert_eq!((status, answer["error"]["code"].as_str()), (507, Some("no_room")), "{answer}");
    assert!(answer["error"]["message"].as_str().unwrap().contains("did not fit"), "{answer}");
    drop(service);
    // A service that has not seen that takes the job, and the vocabulary
    // already there is given again without a second file.
    let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-dir-bytes", &(size + 10).to_string()]);
    let (status, taken) = service.post("/v1/jobs", &job("test:room:a"));
    assert_eq!(status, 202, "{taken}");
    assert_eq!(service.finished(taken["id"].as_str().unwrap())["state"], "done");
    let names: Vec<String> = std::fs::read_dir(dir.join("made")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(std::fs::metadata(dir.join("made").join(&names[0])).unwrap().len(), size);
}

#[test]
fn jobs_are_off_unless_asked_for_and_never_on_an_address_others_can_reach() {
    let dir = scratch("jobs-off");
    let service = Service::start(&dir, &[]);
    assert_eq!(service.said["jobs"], false);
    assert_eq!(service.get("/v1/capabilities").1["jobs"], Value::Null);
    for path in ["/v1/jobs", "/v1/jobs/job-1", "/v1/jobs/job-1/vocabulary"] {
        assert_eq!(service.get(path).0, 404, "{path}");
    }
    assert_eq!(service.post("/v1/jobs", &json!({ "texts": ["x"], "vocabulary_id": "test:a", "vocab_size": 300 })).0, 404);
    assert_eq!(service.post("/v1/train", &json!({})).0, 404);

    let refused = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(&dir).arg("serve").args(args).output().unwrap();
        assert!(!out.status.success(), "{args:?} was accepted");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(refused(&["--listen", "0.0.0.0:0", "--insecure-remote", "--jobs-dir", "made"]).contains("loopback address only"));
    refused(&["--listen", "127.0.0.1:0", "--registry-only", "--jobs-dir", "made"]);
    refused(&["--listen", "127.0.0.1:0", "--jobs-queue", "2"]);
    assert!(!dir.join("made").exists() || std::fs::read_dir(dir.join("made")).unwrap().next().is_none());
}
