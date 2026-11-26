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
    // A job cannot be changed, and there is nothing to withdraw.
    for method in ["PUT", "PATCH"] {
        let (status, head, _) = service.call(method, "/v1/jobs/job-1");
        assert_eq!(status, 405, "{method}");
        assert!(head.contains("Allow: GET, DELETE"), "{head}");
        assert_eq!(service.call(method, "/v1/jobs").0, 405);
    }
    assert_eq!(service.call("DELETE", "/v1/jobs").0, 405, "the list cannot be emptied");
    assert_eq!(service.call("DELETE", "/v1/jobs/job-1").0, 404, "no such job");
    let (status, head, _) = service.call("DELETE", "/v1/jobs/job-1/vocabulary");
    assert_eq!(status, 405, "a file is not withdrawn");
    assert!(head.contains("Allow: GET") && !head.contains("DELETE"), "{head}");
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
/// Text that takes training a while: many different words.
fn slow_text() -> String {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut text = String::new();
    while text.len() < 600_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        for shift in [0, 5, 10, 15, 20, 25] {
            text.push((b'a' + ((seed >> shift) & 15) as u8) as char);
        }
        text.push(' ');
    }
    text
}

fn kute(dir: &Path, args: &[&str]) -> (bool, Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap();
    (out.status.success(), serde_json::from_slice(&out.stdout).unwrap_or(Value::Null), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_command_line_gives_a_job_and_keeps_only_a_file_that_is_what_it_was_told() {
    let dir = scratch("jobs-client");
    std::fs::write(dir.join("text.txt"), TEXT.repeat(30)).unwrap();
    let (ok, by_hand, said) = kute(&dir, &["train", "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "by-hand.kvf"]);
    assert!(ok, "{said}");
    let service = Service::start(&dir, &["--jobs-dir", "made", "--jobs-max-vocab-size", "400"]);
    let at = format!("http://{}", service.address);

    let (ok, report, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "from-job.kvf"]);
    assert!(ok, "{said}");
    assert_eq!(report["job"], "job-1");
    assert_eq!(report["digest"], by_hand["summary"]["digest"], "the service trained what the command line trains");
    assert_eq!(report["digest_recomputed_from_content"], true);
    assert!(report["means"].as_str().unwrap().contains("its word"), "what was not checked is said");
    let (ok, inspected, said) = kute(&dir, &["inspect", "from-job.kvf"]);
    assert!(ok, "{said}");
    assert_eq!(inspected["summary"]["digest"], report["digest"], "{inspected}");

    // An output that exists is this side's mistake: the service is given nothing.
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "from-job.kvf"]);
    assert!(!ok && said.contains("exists"), "{said}");
    // So is a corpus that cannot travel as text.
    std::fs::write(dir.join("bytes.bin"), [0xffu8, 0xfe, 0x00]).unwrap();
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "bytes.bin", "--id", "test:client", "--vocab-size", "300", "--output", "other.kvf"]);
    assert!(!ok && said.contains("not UTF-8"), "{said}");
    assert_eq!(service.get("/v1/jobs").1["jobs"].as_array().unwrap().len(), 1);

    // What the service refuses is passed on in its words, and nothing is written.
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "401", "--output", "refused.kvf"]);
    assert!(!ok && said.contains("answered 422") && said.contains("more than this service trains"), "{said}");
    assert!(!dir.join("refused.kvf").exists());
    // A job that fails when it runs: the reason comes back, and no file.
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--adapter", "jsonl-events", "--output", "failed.kvf"]);
    assert!(!ok && said.contains("failed:"), "{said}");
    assert!(!dir.join("failed.kvf").exists());
    // A corpus larger than the service reads is not sent.
    std::fs::write(dir.join("large.txt"), TEXT.repeat(60_000)).unwrap();
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "large.txt", "--id", "test:client", "--vocab-size", "300", "--output", "large.kvf"]);
    assert!(!ok && said.contains("the service reads at most"), "{said}");
    assert_eq!(service.get("/v1/jobs").1["jobs"].as_array().unwrap().len(), 2, "one done and one failed; nothing else was given");
    // A service that takes no jobs says so before the corpus is sent.
    let plain = Service::start(&dir, &[]);
    let (ok, _, said) = kute(&dir, &["job", "--at", &format!("http://{}", plain.address), "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "plain.kvf"]);
    assert!(!ok && said.contains("takes no jobs"), "{said}");
    let (ok, _, said) = kute(&dir, &["job", "--at", "https://127.0.0.1:1", "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "tls.kvf"]);
    assert!(!ok && said.contains("service address") && said.contains("https needs this side's certificate"), "{said}");
    // Nobody there.
    drop(service);
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "nobody.kvf"]);
    assert!(!ok && said.contains("service:"), "{said}");
}

#[test]
fn what_a_job_trained_is_served_after_a_restart_and_can_be_mirrored() {
    let dir = scratch("jobs-served");
    std::fs::write(dir.join("text.txt"), TEXT.repeat(30)).unwrap();
    let digest = {
        // Written two ways, it is still the one directory (found in review).
        let service = Service::start(&dir, &["--jobs-dir", "made", "--vocab-dir", "./made"]);
        assert_eq!(service.get("/v1/capabilities").1["vocabularies"].as_array().unwrap().len(), 0, "an empty directory is not a mistake");
        let (ok, report, said) = kute(&dir, &["job", "--at", &format!("http://{}", service.address), "--input", "text.txt", "--id", "test:served", "--vocab-size", "300", "--output", "mine.kvf"]);
        assert!(ok, "{said}");
        // Not while it runs: what a service holds is what it was started with.
        assert_eq!(service.post("/v1/encode", &json!({ "vocabulary": "test:served", "text": "The engine" })).0, 422);
        report["digest"].as_str().unwrap().to_string()
    };
    // Started again over the same directory, it holds what was trained, and takes more.
    let service = Service::start(&dir, &["--jobs-dir", "made", "--vocab-dir", "made", "--registry"]);
    let (_, capabilities) = service.get("/v1/capabilities");
    assert_eq!(capabilities["vocabularies"].as_array().unwrap().len(), 1, "{capabilities}");
    let (status, encoded) = service.post("/v1/encode", &json!({ "vocabulary": "test:served", "text": "The engine reads the event log." }));
    assert_eq!(status, 200, "{encoded}");
    assert_eq!(encoded["vocabulary"]["digest"], json!(digest), "{encoded}");
    let (status, decoded) = service.post("/v1/decode", &json!({ "container_base64": encoded["container_base64"] }));
    assert_eq!((status, decoded["text"].as_str()), (200, Some("The engine reads the event log.")), "{decoded}");
    let (ok, _, said) = kute(&dir, &["job", "--at", &format!("http://{}", service.address), "--input", "text.txt", "--id", "test:served:second", "--vocab-size", "290", "--output", "second.kvf"]);
    assert!(ok, "{said}");
    // And it gives what it holds to a mirror, which holds the same file the job's caller got.
    let (ok, mirrored, said) = kute(&dir, &["fetch", "--from", &format!("http://{}", service.address), "--mirror", "copy"]);
    assert!(ok, "{said}");
    assert_eq!((mirrored["listed"].clone(), mirrored["written"][0]["digest"].clone()), (json!(1), json!(digest)), "only what it was started with: {mirrored}");
    let (_, copy, _) = kute(&dir, &["inspect", &format!("copy/{}.kvf", &digest["sha256:".len()..])]);
    let (_, mine, _) = kute(&dir, &["inspect", "mine.kvf"]);
    assert_eq!(copy["summary"], mine["summary"]);
}

/// Something that is not a jobs service: it gives these answers, one a
/// connection, in this order, whatever is asked. Returns its address.
fn scripted(answers: Vec<(u16, Vec<u8>)>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for (status, body) in answers {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut seen = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut wanted = None;
            loop {
                if wanted.is_none() {
                    if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&seen[..at]).to_ascii_lowercase();
                        let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).map_or(0, |v| v.trim().parse::<usize>().unwrap());
                        wanted = Some(at + 4 + length);
                    }
                }
                if wanted.is_some_and(|w| seen.len() >= w) {
                    break;
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&chunk[..n]),
                }
            }
            let mut answer = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
            answer.extend_from_slice(&body);
            let _ = stream.write_all(&answer);
        }
    });
    format!("http://{address}")
}

#[test]
fn a_service_that_lies_to_the_command_line_gets_nothing_written() {
    let dir = scratch("jobs-liar");
    std::fs::write(dir.join("text.txt"), TEXT.repeat(30)).unwrap();
    let train = |id: &str, version: &str, file: &str| {
        let (ok, report, said) = kute(&dir, &["train", "--input", "text.txt", "--id", id, "--vocab-version", version, "--vocab-size", "300", "--output", file]);
        assert!(ok, "{said}");
        (report["summary"]["digest"].as_str().unwrap().to_string(), std::fs::read(dir.join(file)).unwrap())
    };
    let (digest, file) = train("test:liar", "0.1.0", "right.kvf");
    let (other_digest, other_file) = train("test:else", "0.1.0", "else.kvf");
    let (later_digest, later_file) = train("test:liar", "0.2.0", "later.kvf");
    let takes = || (200, json!({ "jobs": {}, "limits": { "max_body_bytes": 3 << 20 } }).to_string().into_bytes());
    let taken = |id: &str| (202, json!({ "id": id, "state": "waiting" }).to_string().into_bytes());
    let done = |digest: &str| (200, json!({ "id": "job-1", "state": "done", "finished_unix": 1, "result": { "digest": digest } }).to_string().into_bytes());
    let ask = |at: &str, extra: &[&str]| {
        let mut args = vec!["job", "--at", at, "--input", "text.txt", "--id", "test:liar", "--vocab-size", "300", "--output", "out.kvf"];
        args.extend_from_slice(extra);
        let (ok, _, said) = kute(&dir, &args);
        assert!(!ok, "{said}");
        assert!(!dir.join("out.kvf").exists(), "a file was written: {said}");
        said
    };

    // The honest sequence, to show the script itself is one the command accepts.
    let at = scripted(vec![takes(), taken("job-1"), done(&digest), (200, file.clone())]);
    let (ok, report, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:liar", "--vocab-size", "300", "--output", "honest.kvf"]);
    assert!(ok && report["digest"] == json!(digest), "{said}");

    // A job name that would be put in a path.
    for id in ["job-1/../../v1/capabilities", "job-1?x=1", "../x", ""] {
        assert!(ask(&scripted(vec![takes(), taken(id)]), &[]).contains("does not name it"), "{id:?}");
    }
    // A digest named, and other content sent.
    let zero = format!("sha256:{}", "0".repeat(64));
    assert!(ask(&scripted(vec![takes(), taken("job-1"), done(&zero), (200, file.clone())]), &[]).contains("whose content has digest"));
    // Content that is what it says it is, and is another vocabulary.
    assert!(ask(&scripted(vec![takes(), taken("job-1"), done(&other_digest), (200, other_file)]), &[]).contains("named test:else, not test:liar"));
    // The name asked for, in another version.
    assert!(ask(&scripted(vec![takes(), taken("job-1"), done(&later_digest), (200, later_file)]), &[]).contains("sent version 0.2.0"));
    // Not a vocabulary at all, a result with no digest, and a file larger than allowed.
    ask(&scripted(vec![takes(), taken("job-1"), done(&digest), (200, b"{}".to_vec())]), &[]);
    assert!(ask(&scripted(vec![takes(), taken("job-1"), done("latest")]), &[]).contains("names no digest"));
    assert!(ask(&scripted(vec![takes(), taken("job-1"), done(&digest), (200, file.clone())]), &["--max-bytes", "100"]).contains("--max-bytes"));
    // A state nobody defined.
    let odd = (200, json!({ "id": "job-1", "state": "paused", "finished_unix": 1 }).to_string().into_bytes());
    assert!(ask(&scripted(vec![takes(), taken("job-1"), odd]), &[]).contains("does not know"));
    // It goes silent after taking the job: said so, within the time one answer may take three times over.
    let said = ask(&scripted(vec![takes(), taken("job-1")]), &["--wait", "2"]);
    assert!(said.contains("could not be asked about") && said.contains("may still end on the service"), "{said}");

    // This side's mistake, found before the service is asked: nowhere to write.
    let at = scripted(vec![]);
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:liar", "--vocab-size", "300", "--output", "nodir/out.kvf"]);
    assert!(!ok && said.contains("is not a directory") && said.contains("was not asked"), "{said}");
}

#[test]
fn a_job_given_from_the_command_line_that_does_not_start_in_time_is_withdrawn() {
    let dir = scratch("jobs-client-wait");
    std::fs::write(dir.join("text.txt"), TEXT.repeat(30)).unwrap();
    let service = Service::start(&dir, &["--jobs-dir", "made"]);
    let at = format!("http://{}", service.address);
    let slow = slow_text();
    // One slow job stands before it. One, not several: this test runs
    // beside the others, and training is what loads the machine.
    let (status, job) = service.post("/v1/jobs", &json!({ "texts": [slow, slow], "vocabulary_id": "test:s:a", "vocab_size": 60_000 }));
    assert_eq!(status, 202, "{job}");
    let (ok, _, said) = kute(&dir, &["job", "--at", &at, "--input", "text.txt", "--id", "test:client", "--vocab-size", "300", "--output", "late.kvf", "--wait", "0"]);
    assert!(!ok && said.contains("job-2 had not started after 0 s and was withdrawn"), "{said}");
    assert!(!dir.join("late.kvf").exists());
    assert_eq!(service.get("/v1/jobs/job-2").1["state"], "withdrawn");
}

#[test]
fn a_waiting_job_is_withdrawn_and_one_that_finished_is_not() {
    let dir = scratch("jobs-withdraw");
    let service = Service::start(&dir, &["--jobs-dir", "made"]);
    let slow = slow_text();
    let give = |id: &str, text: &str, size: u32| {
        let (status, job) = service.post("/v1/jobs", &json!({ "texts": [text], "vocabulary_id": id, "vocab_size": size }));
        assert_eq!(status, 202, "{job}");
        job["id"].as_str().unwrap().to_string()
    };
    // Two slow jobs stand before the third, so it is still in the line
    // when it is withdrawn.
    let first = give("test:w:a", &slow, 20_000);
    let second = give("test:w:b", &slow, 20_000);
    let third = give("test:w:c", TEXT, 300);
    let (status, _, gone) = service.call("DELETE", &format!("/v1/jobs/{third}"));
    assert_eq!(status, 200, "{gone}");
    assert_eq!(gone["state"], "withdrawn", "{gone}");
    let (status, _, again) = service.call("DELETE", &format!("/v1/jobs/{third}"));
    assert_eq!((status, again["error"]["code"].as_str()), (409, Some("not_waiting")), "{again}");

    assert_eq!(service.finished(&first)["state"], "done");
    assert_eq!(service.finished(&second)["state"], "done");
    let (status, _, late) = service.call("DELETE", &format!("/v1/jobs/{first}"));
    assert_eq!(status, 409, "{late}");
    assert!(late["error"]["message"].as_str().unwrap().contains("only a waiting job"), "{late}");
    // The withdrawn job never ran: no start, no file, and nothing of it in the directory.
    let (_, job) = service.get(&format!("/v1/jobs/{third}"));
    assert_eq!(job["state"], "withdrawn");
    assert!(job["started_unix"].is_null() && job["result"].is_null(), "{job}");
    assert_eq!(service.get(&format!("/v1/jobs/{third}/vocabulary")).0, 409);
    let (_, list) = service.get("/v1/jobs");
    assert_eq!(list["directory"]["files"], 2, "{list}");
    assert_eq!(list["jobs"].as_array().unwrap().len(), 3, "a withdrawn job is still listed");
}

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
        assert_eq!(list["directory"], json!({ "files": 0, "bytes": 0, "max_bytes": 1u64 << 30, "did_not_fit_bytes": 0, "taking_jobs": true }));
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
    // The list says so too: under the limit, and taking none.
    let directory = service.get("/v1/jobs").1["directory"].clone();
    assert!(directory["bytes"].as_u64().unwrap() < directory["max_bytes"].as_u64().unwrap(), "{directory}");
    assert!(directory["did_not_fit_bytes"].as_u64().unwrap() > 10, "{directory}");
    assert_eq!(directory["taking_jobs"], false, "{directory}");
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
    assert!(refused(&["--listen", "0.0.0.0:0", "--insecure-remote", "--jobs-dir", "made"]).contains("loopback address, or over TLS"));
    refused(&["--listen", "127.0.0.1:0", "--registry-only", "--jobs-dir", "made"]);
    refused(&["--listen", "127.0.0.1:0", "--jobs-queue", "2"]);
    assert!(!dir.join("made").exists() || std::fs::read_dir(dir.join("made")).unwrap().next().is_none());
}
