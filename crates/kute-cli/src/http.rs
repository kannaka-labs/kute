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
use kute_protocol::tls;
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
    /// Give the files of the held vocabularies to whoever asks, under
    /// `/v1/vocabularies`. Off, those paths do not exist.
    pub registry: bool,
    /// Answer the five operations. Off, the service is a registry and
    /// nothing else: no caller can make it compute.
    pub operations: bool,
    /// Other registries to ask for a vocabulary this one does not hold.
    pub federation: Option<Arc<Federation>>,
    /// The status file of a peer on this machine, read for `GET /v1/peer`.
    pub peer_status: Option<std::path::PathBuf>,
    /// Training jobs, under `/v1/jobs`. `None`, those paths do not exist.
    pub jobs: Option<Arc<crate::jobs::Jobs>>,
    /// TLS with a certificate on both sides. `None`, plain HTTP.
    pub tls: Option<Tls>,
}

/// What makes every connection TLS 1.3 with a certificate on both sides.
#[derive(Clone)]
pub struct Tls {
    /// This side's certificate, and the authorities whose clients are heard.
    pub config: Arc<tls::ServerConfig>,
    /// If some, only clients whose own certificate has one of these
    /// fingerprints are answered; the rest are told so and nothing else.
    pub allow: Option<Arc<std::collections::BTreeSet<String>>>,
    /// The handshake must finish within this, or within the time a
    /// request has if that is shorter. A handshake needs milliseconds
    /// and a request may need seconds, and before the handshake nobody
    /// knows who is calling.
    pub handshake: Duration,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls").field("allow", &self.allow).finish_non_exhaustive()
    }
}

/// A connection, plain or not.
enum Wire {
    Plain(TcpStream),
    Secure(Box<tls::ServerStream>),
}

impl Wire {
    /// The socket, for setting how long a read or a write may take.
    fn socket(&self) -> &TcpStream {
        match self {
            Wire::Plain(tcp) => tcp,
            Wire::Secure(stream) => stream.socket(),
        }
    }

    /// Nothing more will be sent.
    fn end(&mut self) {
        if let Wire::Secure(stream) = self {
            stream.close();
        }
        let _ = self.socket().shutdown(std::net::Shutdown::Write);
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(tcp) => tcp.read(buf),
            Wire::Secure(stream) => stream.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(tcp) => tcp.write(buf),
            Wire::Secure(stream) => stream.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Wire::Plain(tcp) => tcp.flush(),
            Wire::Secure(stream) => stream.flush(),
        }
    }
}

/// Asking other registries, and what that may cost this one.
///
/// A vocabulary this registry does not hold is asked for upstream, in
/// the order the upstreams were named, checked as `kute fetch` checks
/// one (parsed, valid, the digest recomputed, test vectors run), and
/// kept in memory to be given again. Every bound is here: one question
/// upstream at a time, however many callers ask; a deadline and a size
/// limit on each; a cache of fixed size; and a short memory of digests
/// nobody had, so that asking for them again costs nothing.
pub struct Federation {
    pub upstreams: Vec<String>,
    /// Longest wait for one upstream.
    pub timeout: Duration,
    /// Largest file taken from an upstream.
    pub max_bytes: u64,
    /// Most bytes of upstream files kept. The oldest go first.
    pub cache_bytes: u64,
    /// What an upstream file must pass, beyond being the content its
    /// digest names: a signature from a key this registry trusts. With
    /// none, a file is passed on whoever signed it or nobody.
    check: Option<UpstreamCheck>,
    asking: Mutex<()>,
    kept: Mutex<std::collections::VecDeque<(String, Arc<[u8]>)>>,
    absent: Mutex<std::collections::VecDeque<(String, Instant)>>,
    /// For each upstream, when it last failed. One that failed is not
    /// asked again for a while, so a slow or broken upstream named
    /// first costs its timeout once and not on every question.
    failed: Mutex<Vec<Option<Instant>>>,
}

pub type UpstreamCheck = Box<dyn Fn(&kute::Vocabulary) -> Result<(), String> + Send + Sync>;

impl std::fmt::Debug for Federation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Federation").field("upstreams", &self.upstreams).field("check", &self.check.is_some()).finish_non_exhaustive()
    }
}

/// How long a digest that no upstream had is remembered as absent.
const ABSENT_FOR: Duration = Duration::from_secs(60);
/// How long an upstream that failed, stalled or sent something else is left alone.
const REST_FOR: Duration = Duration::from_secs(30);
/// How many such digests are remembered.
const ABSENT_KEPT: usize = 1024;

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Federation {
    pub fn new(upstreams: Vec<String>, timeout: Duration, max_bytes: u64, cache_bytes: u64, check: Option<UpstreamCheck>) -> kute::Result<Self> {
        for upstream in &upstreams {
            crate::registry::parse_url(upstream)?;
        }
        let failed = Mutex::new(vec![None; upstreams.len()]);
        Ok(Self { upstreams, timeout, max_bytes, cache_bytes, check, asking: Mutex::new(()), kept: Default::default(), absent: Default::default(), failed })
    }

    fn cached(&self, digest: &str) -> Option<Arc<[u8]>> {
        locked(&self.kept).iter().find(|(d, _)| d == digest).map(|(_, file)| file.clone())
    }

    /// The file with this digest from the cache or, when `ask` is set,
    /// from the first upstream that gives it. `Ok(None)` when nobody has it.
    fn find(&self, engine: &kute::Engine, digest: &str, ask: bool) -> Result<Option<Arc<[u8]>>, Reply> {
        if let Some(file) = self.cached(digest) {
            return Ok(Some(file));
        }
        if !ask {
            return Ok(None);
        }
        {
            let mut absent = locked(&self.absent);
            absent.retain(|(_, when)| when.elapsed() < ABSENT_FOR);
            if absent.iter().any(|(d, _)| d == digest) {
                return Ok(None);
            }
        }
        // One question upstream at a time. A caller who arrives meanwhile
        // is told to come back, and its worker is free at once: callers
        // asking for digests nobody has cannot hold every worker.
        let _asking = match self.asking.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(problem(503, "Service Unavailable", "busy", "this registry is asking another for a vocabulary; try again")),
        };
        if let Some(file) = self.cached(digest) {
            return Ok(Some(file));
        }
        let mut every_upstream_said_no = true;
        for (i, upstream) in self.upstreams.iter().enumerate() {
            if locked(&self.failed)[i].is_some_and(|when| when.elapsed() < REST_FOR) {
                // Not asked, so it has not said no.
                every_upstream_said_no = false;
                continue;
            }
            match crate::registry::take(engine, upstream, digest, self.max_bytes, self.timeout, true) {
                Ok(crate::registry::Taken::Found(vocabulary, _)) => {
                    // The right content without a signature this registry
                    // requires is not kept: the next upstream may have it signed.
                    if self.check.as_ref().is_some_and(|check| check(&vocabulary).is_err()) {
                        every_upstream_said_no = false;
                        locked(&self.failed)[i] = Some(Instant::now());
                        continue;
                    }
                    // Written out again here, not passed on as it came.
                    let Ok(file) = kute::vocab::to_vec(&vocabulary) else { continue };
                    let file: Arc<[u8]> = file.into();
                    if file.len() as u64 <= self.cache_bytes {
                        let mut kept = locked(&self.kept);
                        kept.push_back((digest.to_string(), file.clone()));
                        while kept.iter().map(|(_, f)| f.len() as u64).sum::<u64>() > self.cache_bytes {
                            kept.pop_front();
                        }
                    }
                    return Ok(Some(file));
                }
                Ok(crate::registry::Taken::Absent(_)) => {}
                // An upstream that failed or sent something else is passed
                // over, and is not taken as having said the file is absent.
                Err(_) => {
                    every_upstream_said_no = false;
                    locked(&self.failed)[i] = Some(Instant::now());
                }
            }
        }
        if every_upstream_said_no {
            let mut absent = locked(&self.absent);
            if absent.len() >= ABSENT_KEPT {
                absent.pop_front();
            }
            absent.push_back((digest.to_string(), Instant::now()));
        }
        Ok(None)
    }
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
        Self { workers: 4, queue: 16, timeout: Duration::from_secs(10), max_body_bytes: 3 << 20, any_host: false, studio: false, registry: false, operations: true, federation: None, peer_status: None, jobs: None, tls: None }
    }
}

struct Reply {
    status: u16,
    reason: &'static str,
    body: Value,
    allow: Option<&'static str>,
    /// A file of the Studio page, as (content type, text), sent instead of `body`.
    page: Option<(&'static str, &'static str)>,
    /// A vocabulary file, sent instead of `body`.
    file: Option<Box<[u8]>>,
}

fn problem(status: u16, reason: &'static str, code: &str, message: impl Into<String>) -> Reply {
    Reply { status, reason, body: json!({ "error": { "code": code, "message": message.into() } }), allow: None, page: None, file: None }
}

fn ok(body: Value) -> Reply {
    Reply { status: 200, reason: "OK", body, allow: None, page: None, file: None }
}

struct Request {
    method: String,
    path: String,
    host: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
    /// Another registry is asking on a caller's behalf.
    via: bool,
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
fn read_request(stream: &mut Wire, options: &Options) -> Result<Request, Reply> {
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
        stream.socket().set_read_timeout(Some(left)).map_err(|_| late())?;
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
    let (mut host, mut content_type, mut length, mut chunked, mut via) = (None, None, None, false, false);
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
            "kute-registry-via" => via = true,
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
        stream.socket().set_read_timeout(Some(left)).map_err(|_| late())?;
        let want = chunk.len().min(length - body.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(bad("the connection closed before the body was complete")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(late()),
        }
    }
    Ok(Request { method: method.to_string(), path, host, content_type, body, via })
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
        held["operations"] = if options.operations { json!(OPERATIONS) } else { json!([]) };
        held["registry"] = json!(options.registry);
        held["peer_status"] = json!(options.peer_status.is_some());
        held["jobs"] = json!(options.jobs.as_ref().map(|j| j.limits()));
        held["limits"]["max_body_bytes"] = json!(options.max_body_bytes);
        held["tls"] = json!(options.tls.is_some());
        return ok(held);
    }
    if operation == "peer" {
        let Some(path) = &options.peer_status else {
            return problem(404, "Not Found", "not_found", "this service was not told of a peer: start it with --peer-status FILE");
        };
        if request.method != "GET" {
            return Reply { allow: Some("GET"), ..problem(405, "Method Not Allowed", "method_not_allowed", "the peer's status is read with GET; nothing can be sent to the peer from here") };
        }
        return ok(crate::peer_status::read(path));
    }
    if operation == "jobs" || operation.starts_with("jobs/") {
        let Some(jobs) = &options.jobs else {
            return problem(404, "Not Found", "not_found", "this service takes no jobs: start it with --jobs-dir DIR");
        };
        let no_job = || problem(404, "Not Found", "not_found", "there is no such job; a finished job is forgotten after enough later ones");
        return match (request.method.as_str(), operation.strip_prefix("jobs/")) {
            ("GET", None) => ok(jobs.list()),
            ("POST", None) => {
                let arguments = match json_body(request) {
                    Ok(v) => v,
                    Err(reply) => return reply,
                };
                match jobs.submit(&arguments) {
                    Ok(job) => Reply { status: 202, reason: "Accepted", ..ok(job) },
                    Err(crate::jobs::Refusal::Bad(why)) => problem(422, "Unprocessable Content", "refused", why),
                    Err(crate::jobs::Refusal::Full(why)) => problem(503, "Service Unavailable", "busy", why),
                    Err(crate::jobs::Refusal::NoRoom(why)) => problem(507, "Insufficient Storage", "no_room", why),
                }
            }
            (_, None) => Reply { allow: Some("GET, POST"), ..problem(405, "Method Not Allowed", "method_not_allowed", "jobs are listed with GET and given with POST") },
            ("GET", Some(rest)) => match rest.split_once('/') {
                None => jobs.get(rest).map_or_else(no_job, ok),
                Some((id, "vocabulary")) => match jobs.file(id) {
                    None => no_job(),
                    Some(Ok(file)) => Reply { file: Some(file.into_boxed_slice()), ..ok(Value::Null) },
                    Some(Err(why)) => problem(409, "Conflict", "not_ready", why),
                },
                Some(_) => no_job(),
            },
            ("DELETE", Some(rest)) if !rest.contains('/') => match jobs.withdraw(rest) {
                None => no_job(),
                Some(Ok(job)) => ok(job),
                Some(Err(why)) => problem(409, "Conflict", "not_waiting", why),
            },
            (_, Some(rest)) if !rest.contains('/') => Reply { allow: Some("GET, DELETE"), ..problem(405, "Method Not Allowed", "method_not_allowed", "a job is read with GET and, while it waits, withdrawn with DELETE; it cannot be changed, or stopped once it runs") },
            (_, Some(_)) => Reply { allow: Some("GET"), ..problem(405, "Method Not Allowed", "method_not_allowed", "a vocabulary a job made is read with GET") },
        };
    }
    if operation == "vocabularies" || operation.starts_with("vocabularies/") {
        // Without the option the paths do not exist, and the answer does
        // not say what is held.
        if !options.registry {
            return problem(404, "Not Found", "not_found", "this service does not give vocabulary files");
        }
        if request.method != "GET" {
            return Reply { allow: Some("GET"), ..problem(405, "Method Not Allowed", "method_not_allowed", "vocabularies are read with GET; nothing can be sent to this registry") };
        }
        let Some(digest) = operation.strip_prefix("vocabularies/") else {
            let mut listing = server.registry_list();
            // What jobs trained since the start is given too: whoever can
            // ask this registry could already read it under the job.
            if let (Some(jobs), Some(held)) = (&options.jobs, listing["vocabularies"].as_array_mut()) {
                for entry in jobs.trained() {
                    if !held.iter().any(|h| h["digest"] == entry["digest"]) {
                        held.push(entry);
                    }
                }
            }
            // Who else this registry asks. What they hold is theirs to say.
            listing["upstreams"] = json!(options.federation.as_ref().map(|f| f.upstreams.clone()).unwrap_or_default());
            listing["upstream_files_need_a_trusted_signature"] = json!(options.federation.as_ref().is_some_and(|f| f.check.is_some()));
            return ok(listing);
        };
        // Only a digest reaches the lookup: a file is named by the hash
        // of its content and by nothing else.
        if !crate::registry::is_digest(digest) {
            return problem(404, "Not Found", "not_found", "a vocabulary is asked for by its digest: sha256: and 64 lowercase hex digits");
        }
        return match server.registry_file(digest) {
            Some(Ok(file)) => Reply { file: Some(file.into_boxed_slice()), ..ok(Value::Null) },
            Some(Err(_)) => problem(500, "Internal Server Error", "internal", "the vocabulary could not be written out"),
            None => {
                if let Some(file) = options.jobs.as_ref().and_then(|jobs| jobs.file_by_digest(digest)) {
                    return Reply { file: Some(file.into_boxed_slice()), ..ok(Value::Null) };
                }
                // Not held here. A registry asking for another is answered
                // from what is here and kept, and no further.
                let found = match &options.federation {
                    Some(federation) => federation.find(server.engine(), digest, !request.via),
                    None => Ok(None),
                };
                match found {
                    Ok(Some(file)) => Reply { file: Some(file.to_vec().into_boxed_slice()), ..ok(Value::Null) },
                    Ok(None) => problem(404, "Not Found", "not_found", "no vocabulary with that digest is held here"),
                    Err(reply) => reply,
                }
            }
        };
    }
    if !OPERATIONS.contains(&operation) || !options.operations {
        return problem(404, "Not Found", "not_found", format!("there is no operation {operation:?}"));
    }
    if request.method != "POST" {
        return Reply { allow: Some("POST"), ..problem(405, "Method Not Allowed", "method_not_allowed", "operations are called with POST") };
    }
    let arguments = match json_body(request) {
        Ok(v) => v,
        Err(reply) => return reply,
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

/// The body of a POST, as JSON.
fn json_body(request: &Request) -> Result<Value, Reply> {
    // A JSON content type cannot be sent across origins by a web page
    // without the browser asking first, and this service never agrees.
    if !request.content_type.as_deref().is_some_and(|t| t == "application/json" || t.starts_with("application/json;")) {
        return Err(problem(415, "Unsupported Media Type", "unsupported_media_type", "send Content-Type: application/json"));
    }
    serde_json::from_slice(&request.body).map_err(|e| problem(400, "Bad Request", "bad_json", format!("the body is not JSON: {e}")))
}

fn answer(stream: &mut Wire, reply: &Reply, options: &Options) {
    let (content_type, body) = match (reply.page, &reply.file) {
        (Some((content_type, text)), _) => (content_type, text.as_bytes().to_vec()),
        (None, Some(file)) => ("application/json", file.to_vec()),
        (None, None) => ("application/json", serde_json::to_vec(&reply.body).unwrap_or_else(|_| b"{}".to_vec())),
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        reply.status,
        reply.reason,
        body.len()
    );
    if reply.status == 503 {
        head.push_str("Retry-After: 1\r\n");
    }
    if reply.page.is_some() {
        head.push_str(&format!("Content-Security-Policy: {STUDIO_POLICY}\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n"));
    }
    if let Some(allow) = reply.allow {
        head.push_str(&format!("Allow: {allow}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.socket().set_write_timeout(Some(options.timeout));
    if stream.write_all(head.as_bytes()).is_ok() && stream.write_all(&body).is_ok() {
        let _ = stream.flush();
    }
    stream.end();
    // A request refused before it was read to its end leaves bytes
    // unread, and closing over unread bytes resets the connection on
    // some systems (seen on macOS), which can take the answer with it.
    // So read what is left and throw it away: at most 1 MiB and half a
    // second, then close whatever the client does. The answer is safe
    // only if all of what was sent is read, so the limit is the largest
    // unread request an answer is guaranteed to survive.
    let started = Instant::now();
    let _ = stream.socket().set_read_timeout(Some(Duration::from_millis(50)));
    let (mut scrap, mut discarded) = ([0u8; 4096], 0usize);
    while discarded < 1_048_576 && started.elapsed() < Duration::from_millis(500) {
        match stream.read(&mut scrap) {
            Ok(0) | Err(_) => break,
            Ok(n) => discarded += n,
        }
    }
}

fn serve_one(server: &Server, stream: TcpStream, options: &Options) {
    let mut refused = None;
    let mut stream = match &options.tls {
        None => Wire::Plain(stream),
        // The handshake has a time of its own, all of it counted: a
        // connection that will not finish one costs a worker that long
        // and no longer. One that fails it is closed and told nothing
        // in HTTP: it has not shown who it is.
        Some(tls) => match tls::accept_by(&tls.config, stream, Instant::now() + tls.handshake.min(options.timeout)) {
            Ok((secured, who)) => {
                if tls.allow.as_ref().is_some_and(|allowed| !allowed.contains(&who.fingerprint)) {
                    refused = Some(problem(403, "Forbidden", "not_allowed", "this certificate is from an authority this service trusts, and is not one of the certificates it answers"));
                }
                Wire::Secure(Box::new(secured))
            }
            Err(_) => return,
        },
    };
    if let Some(reply) = refused {
        return answer(&mut stream, &reply, options);
    }
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
            Err(TrySendError::Full(stream)) => {
                // Answered here, without reading: reading is what a worker is for.
                // Under TLS there is no answer without a handshake, and a
                // handshake is a worker's too: the connection is closed.
                if options.tls.is_none() {
                    let busy = Reply { ..problem(503, "Service Unavailable", "busy", "every worker is busy and the queue is full; try again") };
                    let brief = Options { timeout: Duration::from_secs(1), ..options.clone() };
                    answer(&mut Wire::Plain(stream), &busy, &brief);
                }
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
