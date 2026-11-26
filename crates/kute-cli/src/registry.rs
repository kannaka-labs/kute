//! `kute fetch`: take a vocabulary from a `kute serve --registry` by its
//! digest, and keep it only if it is that vocabulary.
//!
//! The registry is not trusted and neither is the network between. What
//! is asked for is named by the hash of its content, the hash is
//! recomputed here from what arrives, and with `--trusted-key` a
//! signature is checked as `kute verify` checks one. So a registry, or
//! anyone on the path, can refuse, stall or send something else, and
//! something else is not kept. It can also see what was asked for and
//! what was sent: the connection is plain HTTP.
//!
//! The client is written on `std::net` like the service, with every
//! bound in this file: one request, a deadline for the whole exchange, a
//! cap on the header block and on the body.

use crate::sign;
use kute::{vocab, Engine, KuteError};
use serde_json::{json, Value};
use std::io::{Read, Write};
use kute_protocol::tls;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::path::Path;
use std::time::{Duration, Instant};

/// Most bytes of status line and headers in an answer.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Most bytes of a listing, or of the body of a refusal.
const MAX_LISTING_BYTES: u64 = 16 << 20;

/// The registry routes, under the service's `/v1/`.
pub const LIST_PATH: &str = "/v1/vocabularies";

/// A digest as the registry names files: `sha256:` and 64 lowercase hex
/// digits. Nothing else reaches a path.
pub fn is_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn failed(what: impl Into<String>) -> KuteError {
    KuteError::Backend(format!("registry: {}", what.into()))
}

/// Where a service is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Host and port.
    pub authority: String,
    /// The path its routes are under, or nothing.
    pub base: String,
    /// TLS, with a certificate on both sides.
    pub secure: bool,
}

/// This process's certificate and the authorities it trusts, for every
/// `https` address it connects to. One process is one caller, so there
/// is one.
static CLIENT_TLS: OnceLock<Arc<tls::ClientConfig>> = OnceLock::new();

/// Give this process the certificate it shows to an `https` service and
/// the authorities whose services it believes. Before any address is
/// read: an `https` address is refused while there is none.
pub fn client_identity(certificate: &Path, key: &Path, authorities: &Path) -> kute::Result<()> {
    let config = tls::client_config(tls::load_certificates(certificate)?, tls::load_private_key(key)?, tls::load_certificates(authorities)?)?;
    CLIENT_TLS.set(config).map_err(|_| KuteError::InvalidConfig("this side's certificate was given twice".into()))
}

/// The name a service's certificate must carry: the host, without the
/// port and without the brackets of an IPv6 address.
fn host_of(authority: &str) -> &str {
    let host = authority.rsplit_once(':').map_or(authority, |(host, _)| host);
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

/// Where the address points: host and port, the path the routes are
/// under, and whether the connection is TLS.
pub fn parse_url(url: &str) -> kute::Result<Target> {
    let bad = |why: &str| KuteError::InvalidConfig(format!("registry address {url:?}: {why}"));
    let secure = url.starts_with("https://");
    if secure && CLIENT_TLS.get().is_none() {
        return Err(bad("https needs this side's certificate, its key and the authority that issued the other side's: the --tls options. Without them, http: what is fetched is checked against its digest whatever carried it, but a plain connection shows the network what was asked for and what was sent"));
    }
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).ok_or_else(|| bad("expected http://host:port or https://host:port"))?;
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if authority.is_empty() || authority.contains('@') || authority.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
        return Err(bad("expected http://host:port or https://host:port"));
    }
    let path = path.trim_end_matches('/');
    if path.bytes().any(|b| !(b.is_ascii_alphanumeric() || b"-._~/".contains(&b))) {
        return Err(bad("the path may hold letters, digits and - . _ ~ / only"));
    }
    // A bare IPv6 address has colons of its own; in brackets it is told
    // from a port.
    let has_port = authority.rsplit_once(':').is_some_and(|(host, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && (!host.contains(':') || host.ends_with(']')));
    let authority = if has_port { authority.to_string() } else { format!("{authority}:{}", if secure { 443 } else { 80 }) };
    Ok(Target { authority, base: if path.is_empty() { String::new() } else { format!("/{path}") }, secure })
}

/// A read that failed for a reason other than time. Over TLS the reason
/// is kept: a service that does not accept this side's certificate says
/// so in an alert, after the handshake, and that is where it shows.
fn broke(e: &std::io::Error, secure: bool) -> KuteError {
    if secure && e.kind() == std::io::ErrorKind::InvalidData {
        let why: String = e.to_string().chars().filter(|c| !c.is_control()).take(200).collect();
        return failed(format!("the service ended the TLS connection before a complete answer: {why}. If that names a certificate, it is this side's that was not accepted"));
    }
    failed(format!("the connection failed before a complete answer: {}", e.kind()))
}

/// A connection to a service, plain or not.
enum Wire {
    Plain(TcpStream),
    Secure(Box<tls::ClientStream>),
}

impl Wire {
    /// The socket, for setting how long a read or a write may take.
    fn socket(&self) -> &TcpStream {
        match self {
            Wire::Plain(tcp) => tcp,
            Wire::Secure(stream) => stream.socket(),
        }
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

/// One GET. Returns the status and the body, which is at most `max_bytes`.
/// Sent by a registry asking another on a caller's behalf. A registry
/// that sees it answers from what it holds and asks nobody further, so
/// a question travels one registry beyond the one it was put to and
/// registries that name each other cannot pass it round.
pub const VIA_HEADER: &str = "Kute-Registry-Via";

/// Name lookups allowed to be unanswered at once in this process.
const MAX_LOOKUPS: usize = 16;

fn get(at: &Target, path: &str, max_bytes: u64, timeout: Duration, via: bool) -> kute::Result<(u16, Vec<u8>)> {
    request("GET", at, path, None, max_bytes, timeout, via)
}

/// One request, with a JSON body or none. Returns the status and the
/// body, which is at most `max_bytes`. `kute job` uses it for the three
/// methods a jobs service has; the method is this program's own word,
/// never a caller's.
///
/// `path` is under the target's own. To an `https` target the connection
/// is TLS 1.3 with this process's certificate, the service's must be
/// from an authority this process was given and carry the host's name,
/// and the handshake is inside the same time as everything else.
pub fn request(method: &str, at: &Target, path: &str, body: Option<&[u8]>, max_bytes: u64, timeout: Duration, via: bool) -> kute::Result<(u16, Vec<u8>)> {
    let authority = at.authority.as_str();
    let path = &format!("{}{path}", at.base);
    let deadline = Instant::now() + timeout;
    let left = || deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| failed("no complete answer in time"));
    // Looking a name up has no deadline of its own in the standard
    // library, so it is done on another thread and waited for only as
    // long as the exchange has. A lookup that outlasts the wait leaves
    // its thread behind until the resolver of the system gives up. `kute
    // fetch` ends soon after; a registry asking upstream runs for hours,
    // so the number left behind at once is capped.
    static LOOKUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    use std::sync::atomic::Ordering::SeqCst;
    if LOOKUPS.fetch_add(1, SeqCst) >= MAX_LOOKUPS {
        LOOKUPS.fetch_sub(1, SeqCst);
        return Err(failed(format!("{authority} was not looked up: {MAX_LOOKUPS} earlier lookups are still unanswered")));
    }
    let (found, lookup) = std::sync::mpsc::channel();
    let name = authority.to_string();
    std::thread::spawn(move || {
        let _ = found.send(name.to_socket_addrs().map(|addresses| addresses.collect::<Vec<_>>()));
        LOOKUPS.fetch_sub(1, SeqCst);
    });
    let addresses = lookup.recv_timeout(left()?).map_err(|_| failed(format!("{authority} was not looked up in time")))?.map_err(|e| failed(format!("{authority}: {e}")))?;
    let mut stream = None;
    let mut last = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, left()?) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let tcp = stream.ok_or_else(|| failed(format!("could not connect to {authority}: {}", last.map_or("no address".to_string(), |e| e.to_string()))))?;
    let mut stream = if at.secure {
        let config = CLIENT_TLS.get().ok_or_else(|| failed("https, and this side has no certificate"))?;
        left()?;
        // Who answered is the certificate's business: one from a trusted
        // authority, for this host's name. Nothing is sent before that.
        let (secured, _) = tls::connect_by(config, host_of(authority), tcp, deadline).map_err(|e| failed(format!("{authority}: {e}")))?;
        Wire::Secure(Box::new(secured))
    } else {
        Wire::Plain(tcp)
    };
    stream.socket().set_write_timeout(Some(left()?))?;
    let via = if via { format!("{VIA_HEADER}: 1\r\n") } else { String::new() };
    let sending = body.map_or(String::new(), |b| format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\n{sending}{via}Connection: close\r\n\r\n").as_bytes())?;
    if let Some(body) = body {
        // A body is sent in pieces so that the time allowed is for the
        // whole exchange, not for each write of a large corpus.
        for piece in body.chunks(64 * 1024) {
            stream.socket().set_write_timeout(Some(left()?))?;
            if stream.write_all(piece).is_err() {
                // The other side may have answered and closed without
                // reading it all: a refusal of a body too large does.
                // What it said is read below.
                break;
            }
        }
    }
    let mut buffer: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(failed("the answer's headers are larger than any this client reads"));
        }
        stream.socket().set_read_timeout(Some(left()?))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(failed("the connection closed before an answer")),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => return Err(failed("no complete answer in time")),
            Err(e) => return Err(broke(&e, at.secure)),
        }
    };
    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| failed("the answer's headers are not text"))?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|rest| rest.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| failed("the answer is not HTTP"))?;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.trim().parse::<u64>().map_err(|_| failed("Content-Length is not a number"))?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(failed("the answer is in a transfer encoding; this client reads a body of stated length only"));
        }
    }
    // A stated length, so that a body cut short is told from a whole one.
    let length = length.ok_or_else(|| failed("the answer does not state its length"))?;
    if length > max_bytes {
        return Err(KuteError::LimitExceeded(format!("the registry's answer is {length} bytes; the limit is {max_bytes} (--max-bytes)")));
    }
    let mut body = buffer.split_off(head_end + 4);
    if body.len() as u64 > length {
        return Err(failed("more bytes than the stated length"));
    }
    while (body.len() as u64) < length {
        stream.socket().set_read_timeout(Some(left()?))?;
        let want = chunk.len().min((length - body.len() as u64) as usize);
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(failed("the connection closed before the whole answer")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => return Err(failed("no complete answer in time")),
            Err(e) => return Err(broke(&e, at.secure)),
        }
    }
    Ok((status, body))
}

/// The reason a registry gave for not answering 200, kept short: it is
/// text from the other side.
fn refusal(status: u16, body: &[u8]) -> KuteError {
    let said = serde_json::from_slice::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(|m| m.chars().filter(|c| !c.is_control()).take(300).collect::<String>()));
    failed(format!("answered {status}{}", said.map_or(String::new(), |m| format!(": {m}"))))
}

/// What each registry says it holds. Nothing in it is checked: it is the
/// other side's account, and a file is checked when it is fetched.
///
/// One registry is reported as it was before there could be several, and
/// a failure to reach it is an error. Several are reported one row each,
/// a failure being that registry's row.
pub fn list(from: &[String], timeout: Duration) -> kute::Result<Value> {
    let one = |registry: &str| -> kute::Result<Value> {
        let at = parse_url(registry)?;
        let (status, body) = get(&at, LIST_PATH, MAX_LISTING_BYTES, timeout, false)?;
        if status != 200 {
            return Err(refusal(status, &body));
        }
        let listing: Value = serde_json::from_slice(&body).map_err(|_| failed("the listing is not JSON"))?;
        Ok(json!({ "registry": registry, "says_it_holds": listing["vocabularies"], "says_it_asks": listing["upstreams"] }))
    };
    if let [registry] = from {
        let mut row = one(registry)?;
        row["checked"] = json!("nothing: this is what the registry says. A vocabulary is checked when it is fetched by digest.");
        return Ok(row);
    }
    for registry in from {
        parse_url(registry)?;
    }
    let rows: Vec<Value> = from.iter().map(|registry| one(registry).unwrap_or_else(|e| json!({ "registry": registry, "error": e.to_string() }))).collect();
    Ok(json!({
        "registries": rows,
        "checked": "nothing: this is what each registry says. A vocabulary is checked when it is fetched by digest.",
    }))
}

/// What asking one registry for one digest came to.
pub enum Taken {
    /// The vocabulary with that digest, checked, and the bytes it came in.
    Found(Box<kute::Vocabulary>, usize),
    /// The registry answered 404: it does not have it. In its words.
    Absent(String),
}

/// Ask one registry for the vocabulary with this digest and check what
/// arrives: it parses, it is valid, its content has that digest, and its
/// own test vectors pass. Signatures are not looked at here.
pub fn take(engine: &Engine, registry: &str, digest: &str, max_bytes: u64, timeout: Duration, via: bool) -> kute::Result<Taken> {
    let at = parse_url(registry)?;
    let (status, body) = get(&at, &format!("{LIST_PATH}/{digest}"), max_bytes, timeout, via)?;
    if status == 404 {
        return Ok(Taken::Absent(refusal(status, &body[..body.len().min(MAX_LISTING_BYTES as usize)]).to_string()));
    }
    if status != 200 {
        return Err(refusal(status, &body[..body.len().min(MAX_LISTING_BYTES as usize)]));
    }
    let vocabulary = vocab::from_slice(&body)?;
    vocabulary.validate()?;
    // The digest of what arrived, not the one the file states.
    let arrived = vocabulary.compute_digest()?;
    if arrived != digest {
        return Err(KuteError::Integrity(format!("the registry sent a vocabulary with digest {arrived}, not the one asked for")));
    }
    let report = engine.verify(&vocabulary)?;
    if !report.ok() {
        return Err(KuteError::Integrity(format!("the vocabulary fails its own test vectors: {}", report.failures.join("; "))));
    }
    Ok(Taken::Found(Box::new(vocabulary), body.len()))
}

/// Fetch the vocabulary with this digest from the first of the
/// registries that gives it and passes every check, and write it. The
/// registries are asked in the order given; one that fails, lies or
/// does not have it costs its share of the time and nothing else.
#[allow(clippy::too_many_arguments)]
pub fn fetch(engine: &Engine, from: &[String], digest: &str, output: &Path, trusted: &[String], force: bool, max_bytes: u64, timeout: Duration) -> kute::Result<Value> {
    if !is_digest(digest) {
        return Err(KuteError::InvalidConfig(format!("{digest:?} is not a digest: expected sha256: and 64 lowercase hex digits")));
    }
    // Before anything is asked for: a key that cannot be read, an
    // address that cannot be, or an output that may not be written, is
    // this side's mistake.
    let gate = if trusted.is_empty() { None } else { Some(sign::gate(trusted)?) };
    if output.exists() && !force {
        return Err(KuteError::InvalidConfig(format!("{} exists; pass --force to replace it", output.display())));
    }
    for registry in from {
        parse_url(registry)?;
    }
    let mut passed_over: Vec<Value> = Vec::new();
    for registry in from {
        let why = match take(engine, registry, digest, max_bytes, timeout, false) {
            Ok(Taken::Found(vocabulary, bytes)) => match gate.as_ref().map_or(Ok(()), |gate| gate(&vocabulary)) {
                Ok(()) => {
                    let signatures = vocabulary.integrity.as_ref().map_or(0, |i| i.signatures.len());
                    vocab::save(&vocabulary, output, force)?;
                    return Ok(json!({
                        "registry": registry,
                        "registries_passed_over": passed_over,
                        "digest": digest,
                        "digest_recomputed_from_content": true,
                        "id": vocabulary.id().as_str(),
                        "version": vocabulary.version().to_string(),
                        "tokens": vocabulary.len(),
                        "bytes_received": bytes,
                        "test_vectors_pass": true,
                        "signature": if gate.is_some() { "a trusted key signed exactly this content" } else { "not checked: no --trusted-key was given" },
                        // The file is written with whatever signatures arrived. Only one
                        // that a trusted key was seen to hold has been checked; the rest
                        // are entries anyone could have added, the registry included.
                        "signatures_in_the_file": signatures,
                        "signatures_in_the_file_means": if gate.is_some() { "at least one is from a trusted key; any others were not checked" } else { "none was checked: they are claims until `kute verify` is run with keys you trust" },
                        "output": output,
                        "means": "the file is the content this digest names. Whether that content is the one you wanted depends on where the digest came from; who made it is known only if a signature was checked.",
                    }));
                }
                Err(why) => format!("the vocabulary is not kept: {why}"),
            },
            Ok(Taken::Absent(said)) => said,
            Err(e) => e.to_string(),
        };
        passed_over.push(json!({ "registry": registry, "why": why }));
    }
    let said: Vec<String> = passed_over.iter().map(|p| format!("{}: {}", p["registry"].as_str().unwrap_or(""), p["why"].as_str().unwrap_or(""))).collect();
    Err(KuteError::Integrity(format!("nothing was written. {}", said.join(" | "))))
}

/// What a mirror may take.
pub struct MirrorLimits {
    /// Vocabularies asked for, at most. A listing is the other side's
    /// and can be as long as it likes.
    pub max_files: usize,
    /// Bytes written in one run, at most. The file that passes it is
    /// kept and nothing after it is asked for.
    pub max_total_bytes: u64,
    /// Largest single file, as for a fetch.
    pub max_bytes: u64,
}

/// Bring every vocabulary the registries say they hold into `dir`, each
/// as `<64 hex digits>.kvf`, each fetched and checked as `fetch` does.
/// A file already there with the right content is kept and not asked
/// for. Nothing is ever deleted, and a file there under a digest's name
/// that is not that vocabulary is left alone and reported.
///
/// Returns the report, and whether everything listed is now in `dir`.
pub fn mirror(engine: &Engine, from: &[String], dir: &Path, trusted: &[String], limits: &MirrorLimits, timeout: Duration) -> kute::Result<(Value, bool)> {
    let gate = if trusted.is_empty() { None } else { Some(sign::gate(trusted)?) };
    for registry in from {
        parse_url(registry)?;
    }
    std::fs::create_dir_all(dir)?;
    // What each says it holds. A listing is not believed about content,
    // only about what to ask for.
    let mut complete = true;
    let mut registries = Vec::new();
    let mut digests: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut not_digests = 0usize;
    for registry in from {
        let listed = (|| -> kute::Result<Vec<Value>> {
            let at = parse_url(registry)?;
            let (status, body) = get(&at, LIST_PATH, MAX_LISTING_BYTES, timeout, false)?;
            if status != 200 {
                return Err(refusal(status, &body));
            }
            let listing: Value = serde_json::from_slice(&body).map_err(|_| failed("the listing is not JSON"))?;
            listing["vocabularies"].as_array().cloned().ok_or_else(|| failed("the listing names no vocabularies"))
        })();
        match listed {
            Ok(rows) => {
                for row in &rows {
                    match row["digest"].as_str().filter(|d| is_digest(d)) {
                        Some(digest) => {
                            if seen.insert(digest.to_string()) {
                                digests.push(digest.to_string());
                            }
                        }
                        None => not_digests += 1,
                    }
                }
                registries.push(json!({ "registry": registry, "listed": rows.len() }));
            }
            Err(e) => {
                complete = false;
                registries.push(json!({ "registry": registry, "error": e.to_string() }));
            }
        }
    }
    let listed = digests.len();
    let mut written = Vec::new();
    let mut already_there = 0usize;
    let mut failures = Vec::new();
    let mut not_tried = 0usize;
    let mut total = 0u64;
    // Counted where a vocabulary is asked for, not by its place in the
    // listing: what is already there takes none of a run's allowance,
    // or the run after one that stopped at the limit would stop at the
    // same place.
    let mut asked = 0usize;
    for digest in &digests {
        let target = dir.join(format!("{}.kvf", &digest["sha256:".len()..]));
        let mut replace = false;
        if target.exists() {
            // Checked as one that arrives is: valid, its own test
            // vectors, and the digest of its content.
            let there = vocab::load(&target).and_then(|v| {
                v.validate()?;
                let passes = engine.verify(&v)?.ok();
                Ok((v.compute_digest()?, passes, v))
            });
            match there {
                Ok((held, true, vocabulary)) if held == *digest => {
                    if gate.as_ref().is_none_or(|gate| gate(&vocabulary).is_ok()) {
                        already_there += 1;
                        continue;
                    }
                    // The right content with no trusted signature on it:
                    // a registry may have a signed copy.
                    replace = true;
                }
                _ => {
                    complete = false;
                    failures.push(json!({ "digest": digest, "why": format!("{} is there and is not that vocabulary; it was left as it is", target.display()) }));
                    continue;
                }
            }
        }
        if asked >= limits.max_files || total >= limits.max_total_bytes {
            not_tried += 1;
            continue;
        }
        asked += 1;
        match fetch(engine, from, digest, &target, trusted, replace, limits.max_bytes, timeout) {
            Ok(report) => {
                total += report["bytes_received"].as_u64().unwrap_or(0);
                written.push(json!({ "digest": digest, "id": report["id"], "version": report["version"], "bytes": report["bytes_received"], "registry": report["registry"], "file": target }));
            }
            Err(e) => {
                complete = false;
                failures.push(json!({ "digest": digest, "why": e.to_string() }));
            }
        }
    }
    if not_tried > 0 {
        complete = false;
    }
    Ok((
        json!({
            "mirror": dir,
            "registries": registries,
            "listed": listed,
            "listed_without_a_digest": not_digests,
            "written": written,
            "bytes_written": total,
            "already_there": already_there,
            "failed": failures,
            "not_tried": not_tried,
            "not_tried_because": if not_tried > 0 { json!(format!("this run takes at most {} vocabularies and {} bytes (--max-files, --max-total-bytes); run it again to go on", limits.max_files, limits.max_total_bytes)) } else { Value::Null },
            "complete": complete,
            "signature": if gate.is_some() { "every file written or kept carries a signature by a trusted key over its content" } else { "not checked: no --trusted-key was given" },
            "means": "every file written, and every file counted as already there, holds the content its name says and passes its own test vectors. What there was to mirror is the registries' word: one can leave out what it holds. Nothing was deleted: a file in the directory that no registry lists any more is still there.",
        }),
        complete,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_digests_are_read_strictly() {
        let plain = |authority: &str, base: &str| Target { authority: authority.to_string(), base: base.to_string(), secure: false };
        assert_eq!(parse_url("http://127.0.0.1:8787").unwrap(), plain("127.0.0.1:8787", ""));
        assert_eq!(parse_url("http://example.org/kute/").unwrap(), plain("example.org:80", "/kute"));
        assert_eq!((host_of("example.org:443"), host_of("[::1]:9000"), host_of("127.0.0.1:1")), ("example.org", "::1", "127.0.0.1"));
        assert_eq!(parse_url("http://[::1]:9000").unwrap().authority, "[::1]:9000");
        assert_eq!(parse_url("http://[::1]").unwrap().authority, "[::1]:80");
        for bad in ["https://example.org", "example.org:80", "http://", "http://user@host", "http://host/a b", "http://host/x?y=1", "http://host/\r\nX: 1"] {
            assert!(parse_url(bad).is_err(), "{bad:?}");
        }
        assert!(is_digest(&format!("sha256:{}", "0a".repeat(32))));
        for bad in [format!("sha256:{}", "0A".repeat(32)), format!("sha256:{}", "0a".repeat(31)), format!("sha256:{}/..", "0a".repeat(31)), "0a".repeat(32)] {
            assert!(!is_digest(&bad), "{bad:?}");
        }
    }
}
