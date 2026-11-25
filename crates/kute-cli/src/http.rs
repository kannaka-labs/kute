//! A small HTTP service over the same five read-only operations as the MCP
//! server, and through the same code: `encode`, `decode`, `inspect`,
//! `compare`, `discover`, plus `capabilities`. Version 1 of the interface
//! lives under `/v1/`.
//!
//! It is written directly on `std::net` so that every bound is in this
//! file: one request per connection, a fixed number of workers, a fixed
//! queue, a deadline on reading, a cap on the header block and on the body.
//! It does not speak TLS and does not authenticate anyone, so it listens
//! on the loopback interface unless told otherwise, and on loopback it
//! answers only requests addressed to a loopback host name.

use crate::mcp::Server;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The operations a POST may name.
pub const OPERATIONS: [&str; 5] = ["encode", "decode", "inspect", "compare", "discover"];

/// Most bytes of request line and headers.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// What the service may cost, apart from the per-call limits of the
/// operations themselves.
#[derive(Clone, Debug)]
pub struct Options {
    /// Requests handled at once.
    pub workers: usize,
    /// Accepted connections that may wait for a worker. Past it, a
    /// connection is answered 503 and closed.
    pub queue: usize,
    /// A request must arrive whole within this time, and its answer must
    /// be taken within it.
    pub timeout: Duration,
    /// Longest request body.
    pub max_body_bytes: u64,
    /// Answer requests whatever host they name. Off, only loopback names
    /// are answered, which keeps a web page from reaching the service
    /// through a name it controls.
    pub any_host: bool,
    /// Serve the Studio page under `/studio`.
    pub studio: bool,
}

/// The Studio page: three files compiled into the binary. The page loads
/// nothing else and calls only `/v1/` on the origin it came from.
const STUDIO: [(&str, &str, &str); 3] = [
    ("/studio", "text/html; charset=utf-8", include_str!("../studio/index.html")),
    ("/studio/studio.css", "text/css; charset=utf-8", include_str!("../studio/studio.css")),
    ("/studio/studio.js", "text/javascript; charset=utf-8", include_str!("../studio/studio.js")),
];

/// What the page may do, enforced by the browser: scripts and styles from
/// this origin only, requests to this origin only, no frames, no forms
/// posting elsewhere, nothing else at all.
const STUDIO_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

impl Default for Options {
    fn default() -> Self {
        Self { workers: 4, queue: 16, timeout: Duration::from_secs(10), max_body_bytes: 3 << 20, any_host: false, studio: false }
    }
}

struct Reply {
    status: u16,
    reason: &'static str,
    body: Value,
    allow: Option<&'static str>,
    /// A file of the Studio page, as (content type, text), sent instead of `body`.
    page: Option<(&'static str, &'static str)>,
}

fn problem(status: u16, reason: &'static str, code: &str, message: impl Into<String>) -> Reply {
    Reply { status, reason, body: json!({ "error": { "code": code, "message": message.into() } }), allow: None, page: None }
}

fn ok(body: Value) -> Reply {
    Reply { status: 200, reason: "OK", body, allow: None, page: None }
}

struct Request {
    method: String,
    path: String,
    host: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn loopback_name(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        // [::1] or [::1]:port, and nothing else after the bracket
        match rest.split_once(']') {
            Some((name, after)) if after.is_empty() || after.strip_prefix(':').is_some_and(|port| port.chars().all(|c| c.is_ascii_digit())) => name,
            _ => return false,
        }
    } else {
        host.rsplit_once(':').map_or(host, |(name, port)| if port.chars().all(|c| c.is_ascii_digit()) { name } else { host })
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Read one request within the deadline and the size limits.
fn read_request(stream: &mut TcpStream, options: &Options) -> Result<Request, Reply> {
    let deadline = Instant::now() + options.timeout;
    let late = || problem(408, "Request Timeout", "timeout", "the request did not arrive in time");
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(problem(431, "Request Header Fields Too Large", "headers_too_large", format!("the request line and headers may take {MAX_HEAD_BYTES} bytes")));
        }
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(late)?;
        stream.set_read_timeout(Some(left)).map_err(|_| late())?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(problem(400, "Bad Request", "bad_request", "the connection closed before the request was complete")),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(late()),
        }
    };
    if head_end > MAX_HEAD_BYTES {
        return Err(problem(431, "Request Header Fields Too Large", "headers_too_large", format!("the request line and headers may take {MAX_HEAD_BYTES} bytes")));
    }
    let bad = |what: &str| problem(400, "Bad Request", "bad_request", what.to_string());
    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| bad("the request line and headers are not text"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target, version) = (first.next().unwrap_or(""), first.next().unwrap_or(""), first.next().unwrap_or(""));
    if method.is_empty() || !target.starts_with('/') || !matches!(version, "HTTP/1.0" | "HTTP/1.1") || first.next().is_some() {
        return Err(bad("the request line is not HTTP/1.x"));
    }
    let (mut host, mut content_type, mut length, mut chunked) = (None, None, None, false);
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| bad("a header line has no colon"))?;
        // A name is a token: no space before the colon, no continuation
        // line. Something in front of this service that read such a line
        // another way would disagree with it about where the request ends.
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
            return Err(bad("a header name is not a token"));
        }
        let value = value.trim_matches([' ', '\t']);
        if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return Err(bad("a header value holds a control character"));
        }
        match name.to_ascii_lowercase().as_str() {
            "host" => {
                if host.is_some() {
                    return Err(bad("two Host headers"));
                }
                host = Some(value.to_string());
            }
            "content-type" => content_type = Some(value.to_ascii_lowercase()),
            "content-length" => {
                // Digits only: `parse` alone would take "+5".
                let n: u64 = Some(value).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse().ok()).ok_or_else(|| bad("Content-Length is not a number"))?;
                if length.is_some_and(|earlier| earlier != n) {
                    return Err(bad("two different Content-Length headers"));
                }
                length = Some(n);
            }
            "transfer-encoding" => chunked = true,
            _ => {}
        }
    }
    if chunked {
        return Err(problem(411, "Length Required", "length_required", "send the body with Content-Length, not a transfer encoding"));
    }
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut body = buffer[head_end + 4..].to_vec();
    let length = match (method, length) {
        (_, Some(n)) => n,
        ("POST", None) => return Err(problem(411, "Length Required", "length_required", "a POST needs Content-Length")),
        (_, None) => 0,
    };
    if length > options.max_body_bytes {
        return Err(problem(413, "Content Too Large", "too_large", format!("the body is {length} bytes; the limit is {}", options.max_body_bytes)));
    }
    let length = length as usize;
    if body.len() > length {
        return Err(bad("more bytes than Content-Length; one request per connection"));
    }
    while body.len() < length {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(late)?;
        stream.set_read_timeout(Some(left)).map_err(|_| late())?;
        let want = chunk.len().min(length - body.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(bad("the connection closed before the body was complete")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(late()),
        }
    }
    Ok(Request { method: method.to_string(), path, host, content_type, body })
}

fn route(server: &Server, request: &Request, options: &Options) -> Reply {
    if !options.any_host && !request.host.as_deref().is_some_and(loopback_name) {
        return problem(403, "Forbidden", "forbidden_host", "this service answers only requests addressed to localhost");
    }
    if let Some((_, content_type, text)) = STUDIO.iter().find(|(path, _, _)| options.studio && *path == request.path) {
        if request.method != "GET" {
            return Reply { allow: Some("GET"), ..problem(405, "Method Not Allowed", "method_not_allowed", "the page is read with GET") };
        }
        return Reply { page: Some((content_type, text)), ..ok(Value::Null) };
    }
    let operation = match request.path.strip_prefix("/v1/") {
        Some(rest) => rest,
        None => return problem(404, "Not Found", "not_found", "the interface is under /v1/"),
    };
    if operation == "capabilities" {
        if request.method != "GET" {
            return Reply { allow: Some("GET"), ..problem(405, "Method Not Allowed", "method_not_allowed", "capabilities is read with GET") };
        }
        let mut held = match server.call("inspect", &json!({})) {
            Ok(v) => v,
            Err(message) => return problem(500, "Internal Server Error", "internal", message),
        };
        held["api"] = json!("v1");
        held["software"] = json!(format!("kute {}", kute::KUTE_VERSION));
        held["operations"] = json!(OPERATIONS);
        held["limits"]["max_body_bytes"] = json!(options.max_body_bytes);
        return ok(held);
    }
    if !OPERATIONS.contains(&operation) {
        return problem(404, "Not Found", "not_found", format!("there is no operation {operation:?}"));
    }
    if request.method != "POST" {
        return Reply { allow: Some("POST"), ..problem(405, "Method Not Allowed", "method_not_allowed", "operations are called with POST") };
    }
    // A JSON content type cannot be sent across origins by a web page
    // without the browser asking first, and this service never agrees.
    if !request.content_type.as_deref().is_some_and(|t| t == "application/json" || t.starts_with("application/json;")) {
        return problem(415, "Unsupported Media Type", "unsupported_media_type", "send Content-Type: application/json");
    }
    let arguments: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(e) => return problem(400, "Bad Request", "bad_json", format!("the body is not JSON: {e}")),
    };
    // A panic in an operation must not end the worker: with every worker
    // gone the service would accept connections and answer none. No input
    // is known to cause one.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.call(operation, &arguments))) {
        Ok(Ok(result)) => ok(result),
        Ok(Err(message)) => problem(422, "Unprocessable Content", "refused", message),
        Err(_) => problem(500, "Internal Server Error", "internal", "the operation failed unexpectedly"),
    }
}

fn answer(stream: &mut TcpStream, reply: &Reply, options: &Options) {
    let (content_type, body) = match reply.page {
        Some((content_type, text)) => (content_type, text.as_bytes().to_vec()),
        None => ("application/json", serde_json::to_vec(&reply.body).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        reply.status,
        reply.reason,
        body.len()
    );
    if reply.page.is_some() {
        head.push_str(&format!("Content-Security-Policy: {STUDIO_POLICY}\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n"));
    }
    if let Some(allow) = reply.allow {
        head.push_str(&format!("Allow: {allow}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.set_write_timeout(Some(options.timeout));
    if stream.write_all(head.as_bytes()).is_ok() && stream.write_all(&body).is_ok() {
        let _ = stream.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // A request refused before it was read to its end leaves bytes
    // unread, and closing over unread bytes resets the connection on
    // some systems (seen on macOS), which can take the answer with it.
    // So read a little of what is left and throw it away: at most 64 KiB
    // and a quarter of a second, then close whatever the client does.
    let started = Instant::now();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let (mut scrap, mut discarded) = ([0u8; 4096], 0usize);
    while discarded < 65_536 && started.elapsed() < Duration::from_millis(250) {
        match stream.read(&mut scrap) {
            Ok(0) | Err(_) => break,
            Ok(n) => discarded += n,
        }
    }
}

fn serve_one(server: &Server, mut stream: TcpStream, options: &Options) {
    let reply = match read_request(&mut stream, options) {
        Ok(request) => route(server, &request, options),
        Err(reply) => reply,
    };
    answer(&mut stream, &reply, options);
}

fn work(server: Arc<Server>, jobs: Arc<Mutex<Receiver<TcpStream>>>, options: Options) {
    loop {
        let next = jobs.lock().map(|queue| queue.recv());
        match next {
            Ok(Ok(stream)) => serve_one(&server, stream, &options),
            _ => return,
        }
    }
}

/// Serve until the listener fails. Each connection carries one request.
pub fn serve(server: Server, listener: TcpListener, options: Options) -> kute::Result<()> {
    let server = Arc::new(server);
    let (send, receive) = sync_channel::<TcpStream>(options.queue.max(1));
    let receive = Arc::new(Mutex::new(receive));
    for _ in 0..options.workers.max(1) {
        let (server, receive, options) = (server.clone(), receive.clone(), options.clone());
        std::thread::spawn(move || work(server, receive, options));
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        match send.try_send(stream) {
            Ok(()) => {}
            Err(TrySendError::Full(mut stream)) => {
                // Answered here, without reading: reading is what a worker is for.
                let busy = Reply { ..problem(503, "Service Unavailable", "busy", "every worker is busy and the queue is full; try again") };
                let brief = Options { timeout: Duration::from_secs(1), ..options.clone() };
                answer(&mut stream, &busy, &brief);
            }
            Err(TrySendError::Disconnected(_)) => return Err(kute::KuteError::Backend("the workers stopped".into())),
        }
    }
    Ok(())
}

/// Whether an address is on the loopback interface.
pub fn is_loopback(address: &SocketAddr) -> bool {
    address.ip().is_loopback()
}
