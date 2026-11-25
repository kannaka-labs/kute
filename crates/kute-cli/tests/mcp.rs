//! Gate AC-707 of the v0.7 specification: "MCP exposes encode, decode,
//! discover, compare and inspect." A real `kute mcp` process is driven over
//! its standard input and output, one JSON-RPC message per line.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start(dir: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).arg("mcp").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        Self { child, stdin, stdout, next: 1 }
    }

    fn line(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn reply(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not a JSON line ({e}): {line:?}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string());
        let reply = self.reply();
        assert_eq!(reply["id"], id, "{reply}");
        assert_eq!(reply["jsonrpc"], "2.0");
        reply
    }

    /// Call a tool and return its structured result, or the refusal text.
    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let reply = self.request("tools/call", json!({ "name": name, "arguments": arguments }));
        let result = &reply["result"];
        assert!(result.is_object(), "{reply}");
        if result["isError"] == true {
            return Err(result["content"][0]["text"].as_str().unwrap().to_string());
        }
        // The text form is the same value, for clients that read only text.
        assert_eq!(serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(), result["structuredContent"]);
        Ok(result["structuredContent"].clone())
    }
}

#[test]
fn the_five_tools_work_over_standard_input_and_output() {
    let dir = scratch("mcp");
    let cycle = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];
    let log: String = cycle.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect();
    std::fs::write(dir.join("log.jsonl"), log.repeat(40)).unwrap();
    std::fs::write(dir.join("text.txt"), "The engine reads the event log. The engine writes the event log.\n".repeat(30)).unwrap();
    kute(&dir, &["train", "--input", "log.jsonl", "--id", "test:mcp:events", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", "24", "--output", "events.kvf"]);
    kute(&dir, &["evolve", "--vocab", "events.kvf", "--add", "VALVE_STUCK", "--new-version", "0.2.0", "--output", "events-0.2.kvf"]);
    kute(&dir, &["train", "--input", "text.txt", "--id", "test:mcp:text", "--vocab-size", "300", "--output", "text.kvf"]);
    std::fs::write(dir.join("frames.bin"), [0xffu8, 0x00, 0x80, 0xfe, 0x01, 0x02].repeat(200)).unwrap();
    kute(&dir, &["train", "--input", "frames.bin", "--id", "test:mcp:frames", "--adapter", "binary", "--vocab-size", "270", "--output", "frames.kvf"]);

    let mut c = Client::start(&dir, &["--vocab", "events.kvf", "--vocab", "events-0.2.kvf", "--vocab", "text.kvf", "--vocab", "frames.kvf", "--max-input-bytes", "4096", "--max-output-bytes", "4096"]);

    // The handshake, and the list of tools with a schema each.
    let hello = c.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    assert_eq!(hello["result"]["serverInfo"]["name"], "kute");
    assert!(hello["result"]["capabilities"]["tools"].is_object());
    c.line(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string()); // no reply
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));
    let tools = c.request("tools/list", json!({}));
    let mut names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names, ["compare", "decode", "discover", "encode", "inspect"], "exactly the five: nothing trains or publishes");
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert!(tool["description"].as_str().unwrap().len() > 40);
    }

    // inspect: the list, a summary, one token.
    let listed = c.tool("inspect", json!({})).unwrap();
    assert_eq!(listed["vocabularies"].as_array().unwrap().len(), 4);
    assert_eq!(listed["limits"]["max_input_bytes"], 4096);
    let summary = c.tool("inspect", json!({ "vocabulary": "test:mcp:text", "longest": 2 })).unwrap();
    assert_eq!(summary["summary"]["algorithm"], "bpe");
    assert_eq!(summary["longest_learned_tokens"].as_array().unwrap().len(), 2);
    let child = c.tool("inspect", json!({ "vocabulary": "test:mcp:events 0.2.0" })).unwrap();
    let parent = c.tool("inspect", json!({ "vocabulary": "test:mcp:events@0.1.0" })).unwrap();
    assert_eq!(child["summary"]["parent"], parent["summary"]["digest"], "lineage is visible through the tool");
    assert!(c.tool("inspect", json!({ "vocabulary": "test:mcp:text", "token": 65 })).unwrap().is_object());
    assert!(c.tool("inspect", json!({ "vocabulary": "test:mcp:events" })).unwrap_err().contains("names 2"), "an id with two versions is ambiguous");

    // encode then decode, text and bytes; the container is all decode needs.
    let text = "The engine reads the log. Naïve café 🙂";
    let encoded = c.tool("encode", json!({ "vocabulary": "test:mcp:text", "text": text })).unwrap();
    assert_eq!(encoded["tokens"], encoded["ids"].as_array().unwrap().len());
    assert!(encoded["tokens"].as_u64().unwrap() < text.len() as u64);
    let decoded = c.tool("decode", json!({ "container_base64": encoded["container_base64"] })).unwrap();
    assert_eq!(decoded["text"], text);
    assert_eq!(decoded["vocabulary"]["digest"], summary["summary"]["digest"]);
    assert!(c.tool("encode", json!({ "vocabulary": "test:mcp:text", "data_base64": "/wCA/g==" })).unwrap_err().contains("UTF-8"), "a text vocabulary takes text");
    let bytes = c.tool("encode", json!({ "vocabulary": "test:mcp:frames", "data_base64": "/wCA/g==" })).unwrap();
    assert_eq!(c.tool("decode", json!({ "container_base64": bytes["container_base64"] })).unwrap()["data_base64"], "/wCA/g==");
    // Events: strict refuses an unknown event, escape carries it.
    let novel = "{\"event\":\"MOTOR_START\"}\n{\"event\":\"NEVER_SEEN\"}\n";
    let digest = parent["summary"]["digest"].as_str().unwrap();
    assert!(c.tool("encode", json!({ "vocabulary": digest, "text": novel })).is_err());
    let escaped = c.tool("encode", json!({ "vocabulary": digest, "text": novel, "unknown": "escape" })).unwrap();
    assert_eq!(escaped["escaped_symbols"], json!(["NEVER_SEEN"]));
    assert_eq!(c.tool("decode", json!({ "container_base64": escaped["container_base64"] })).unwrap()["text"], novel);

    // compare: the evolved vocabulary holds everything its parent does.
    let compared = c.tool("compare", json!({ "a": "test:mcp:events 0.1.0", "b": "test:mcp:events 0.2.0" })).unwrap();
    assert_eq!(compared["only_in_a"], 0);
    assert_eq!(compared["only_in_b"], 1);

    // discover: the cycle is found in records passed in the call.
    let records: Vec<Vec<&str>> = (0..30).map(|_| cycle.to_vec()).collect();
    let found = c.tool("discover", json!({ "records": records, "min_support": 5 })).unwrap();
    assert!(found["patterns"].as_array().unwrap().iter().any(|p| p["symbols"] == json!(cycle)), "{found}");
    let by_lines = c.tool("discover", json!({ "lines": format!("{}\n", cycle.join(" ")).repeat(30), "max_patterns": 3 })).unwrap();
    assert!(by_lines["patterns"].as_array().unwrap().len() <= 6);

    // Refusals are results the caller can read, and the server goes on.
    assert!(c.tool("encode", json!({ "vocabulary": "test:mcp:text", "text": "x".repeat(5000) })).unwrap_err().contains("limit"));
    assert!(c.tool("encode", json!({ "vocabulary": "nobody", "text": "x" })).unwrap_err().contains("no vocabulary"));
    assert!(c.tool("encode", json!({ "vocabulary": "test:mcp:text" })).unwrap_err().contains("exactly one"));
    assert!(c.tool("decode", json!({ "container_base64": "AAAA" })).is_err());
    assert!(c.tool("decode", json!({ "container_base64": "not base64!" })).unwrap_err().contains("base64"));
    assert!(c.tool("discover", json!({ "lines": "A B\n".repeat(2000) })).unwrap_err().contains("limit"));
    let long = c.tool("encode", json!({ "vocabulary": "test:mcp:text", "text": "The engine reads. ".repeat(200) })).unwrap();
    assert!(c.tool("decode", json!({ "container_base64": long["container_base64"] })).unwrap()["bytes"].as_u64().unwrap() == 3600);
    // A container made elsewhere that decodes past the output limit.
    let mut wide = Client::start(&dir, &["--vocab", "text.kvf", "--max-input-bytes", "65536"]);
    let big = wide.tool("encode", json!({ "vocabulary": "test:mcp:text", "text": "The engine reads the event log. ".repeat(400) })).unwrap();
    assert!(c.tool("decode", json!({ "container_base64": big["container_base64"] })).unwrap_err().contains("limit"));
    // Protocol errors: an unknown tool, an unknown method, and a line that
    // is not JSON each get an error and do not end the session.
    assert_eq!(c.request("tools/call", json!({ "name": "train", "arguments": {} }))["error"]["code"], -32602);
    assert_eq!(c.request("resources/list", json!({}))["error"]["code"], -32601);
    c.line("{ this is not json");
    assert_eq!(c.reply()["error"]["code"], -32700);
    c.line("[1, 2]");
    assert_eq!(c.reply()["error"]["code"], -32600);
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));

    // Closing the input ends the server cleanly, and it wrote no file.
    drop(c.stdin);
    assert!(c.child.wait().unwrap().success());
    let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, ["events-0.2.kvf", "events.kvf", "frames.bin", "frames.kvf", "log.jsonl", "text.kvf", "text.txt"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A request line past the limit ends the session with an error, instead of
/// being buffered whole.
#[test]
fn an_oversized_request_ends_the_session() {
    let dir = scratch("mcp-big");
    let mut c = Client::start(&dir, &["--max-input-bytes", "1024"]);
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));
    // The limit on a line is twice the input limit plus one mebibyte.
    let big = format!("{{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}", "x".repeat(2 << 20));
    // The server may stop reading before all of it is written.
    let _ = c.stdin.write_all(big.as_bytes());
    let _ = c.stdin.write_all(b"\n");
    let _ = c.stdin.flush();
    assert_eq!(c.reply()["error"]["code"], -32600);
    drop(c.stdin);
    assert!(!c.child.wait().unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}
