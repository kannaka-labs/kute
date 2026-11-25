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
use std::net::{TcpStream, ToSocketAddrs};
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

/// Host and port, and the path the registry's routes are under.
fn parse_url(url: &str) -> kute::Result<(String, String)> {
    let bad = |why: &str| KuteError::InvalidConfig(format!("registry address {url:?}: {why}"));
    if url.starts_with("https://") {
        return Err(bad("https is not built. What is fetched is checked against its digest whatever carried it, but a plain connection shows the network what was asked for and what was sent"));
    }
    let rest = url.strip_prefix("http://").ok_or_else(|| bad("expected http://host:port"))?;
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if authority.is_empty() || authority.contains('@') || authority.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
        return Err(bad("expected http://host:port"));
    }
    let path = path.trim_end_matches('/');
    if path.bytes().any(|b| !(b.is_ascii_alphanumeric() || b"-._~/".contains(&b))) {
        return Err(bad("the path may hold letters, digits and - . _ ~ / only"));
    }
    // A bare IPv6 address has colons of its own; in brackets it is told
    // from a port.
    let has_port = authority.rsplit_once(':').is_some_and(|(host, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && (!host.contains(':') || host.ends_with(']')));
    let authority = if has_port { authority.to_string() } else { format!("{authority}:80") };
    Ok((authority, if path.is_empty() { String::new() } else { format!("/{path}") }))
}

/// One GET. Returns the status and the body, which is at most `max_bytes`.
fn get(authority: &str, path: &str, max_bytes: u64, timeout: Duration) -> kute::Result<(u16, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    let left = || deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| failed("no complete answer in time"));
    let mut stream = None;
    let mut last = None;
    for address in authority.to_socket_addrs().map_err(|e| failed(format!("{authority}: {e}")))? {
        match TcpStream::connect_timeout(&address, left()?) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let mut stream = stream.ok_or_else(|| failed(format!("could not connect to {authority}: {}", last.map_or("no address".to_string(), |e| e.to_string()))))?;
    stream.set_write_timeout(Some(left()?))?;
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut buffer: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(failed("the answer's headers are larger than any this client reads"));
        }
        stream.set_read_timeout(Some(left()?))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(failed("the connection closed before an answer")),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(failed("no complete answer in time")),
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
        stream.set_read_timeout(Some(left()?))?;
        let want = chunk.len().min((length - body.len() as u64) as usize);
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(failed("the connection closed before the whole answer")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(failed("no complete answer in time")),
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

/// What the registry says it holds. Nothing in it is checked: it is the
/// other side's account, and a file is checked when it is fetched.
pub fn list(from: &str, timeout: Duration) -> kute::Result<Value> {
    let (authority, base) = parse_url(from)?;
    let (status, body) = get(&authority, &format!("{base}{LIST_PATH}"), MAX_LISTING_BYTES, timeout)?;
    if status != 200 {
        return Err(refusal(status, &body));
    }
    let listing: Value = serde_json::from_slice(&body).map_err(|_| failed("the listing is not JSON"))?;
    Ok(json!({
        "registry": from,
        "says_it_holds": listing["vocabularies"],
        "checked": "nothing: this is what the registry says. A vocabulary is checked when it is fetched by digest.",
    }))
}

/// Fetch the vocabulary with this digest, check it, and write it.
#[allow(clippy::too_many_arguments)]
pub fn fetch(engine: &Engine, from: &str, digest: &str, output: &Path, trusted: &[String], force: bool, max_bytes: u64, timeout: Duration) -> kute::Result<Value> {
    if !is_digest(digest) {
        return Err(KuteError::InvalidConfig(format!("{digest:?} is not a digest: expected sha256: and 64 lowercase hex digits")));
    }
    // Before anything is asked for: a key that cannot be read, or an
    // output that may not be written, is this side's mistake.
    let gate = if trusted.is_empty() { None } else { Some(sign::gate(trusted)?) };
    if output.exists() && !force {
        return Err(KuteError::InvalidConfig(format!("{} exists; pass --force to replace it", output.display())));
    }
    let (authority, base) = parse_url(from)?;
    let (status, body) = get(&authority, &format!("{base}{LIST_PATH}/{digest}"), max_bytes, timeout)?;
    if status != 200 {
        return Err(refusal(status, &body[..body.len().min(MAX_LISTING_BYTES as usize)]));
    }
    let vocabulary = vocab::from_slice(&body)?;
    vocabulary.validate()?;
    // The digest of what arrived, not the one the file states.
    let arrived = vocabulary.compute_digest()?;
    if arrived != digest {
        return Err(KuteError::Integrity(format!("the registry sent a vocabulary with digest {arrived}, not the one asked for; nothing was written")));
    }
    let report = engine.verify(&vocabulary)?;
    if !report.ok() {
        return Err(KuteError::Integrity(format!("the vocabulary fails its own test vectors: {}; nothing was written", report.failures.join("; "))));
    }
    if let Some(gate) = &gate {
        gate(&vocabulary).map_err(|why| KuteError::Integrity(format!("the vocabulary is not kept: {why}; nothing was written")))?;
    }
    vocab::save(&vocabulary, output, force)?;
    Ok(json!({
        "registry": from,
        "digest": digest,
        "digest_recomputed_from_content": true,
        "id": vocabulary.id().as_str(),
        "version": vocabulary.version().to_string(),
        "tokens": vocabulary.len(),
        "bytes_received": body.len(),
        "test_vectors_pass": true,
        "signature": if gate.is_some() { "a trusted key signed exactly this content" } else { "not checked: no --trusted-key was given" },
        "output": output,
        "means": "the file is the content this digest names. Whether that content is the one you wanted depends on where the digest came from; who made it is known only if a signature was checked.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_digests_are_read_strictly() {
        assert_eq!(parse_url("http://127.0.0.1:8787").unwrap(), ("127.0.0.1:8787".to_string(), String::new()));
        assert_eq!(parse_url("http://example.org/kute/").unwrap(), ("example.org:80".to_string(), "/kute".to_string()));
        assert_eq!(parse_url("http://[::1]:9000").unwrap().0, "[::1]:9000");
        assert_eq!(parse_url("http://[::1]").unwrap().0, "[::1]:80");
        for bad in ["https://example.org", "example.org:80", "http://", "http://user@host", "http://host/a b", "http://host/x?y=1", "http://host/\r\nX: 1"] {
            assert!(parse_url(bad).is_err(), "{bad:?}");
        }
        assert!(is_digest(&format!("sha256:{}", "0a".repeat(32))));
        for bad in [format!("sha256:{}", "0A".repeat(32)), format!("sha256:{}", "0a".repeat(31)), format!("sha256:{}/..", "0a".repeat(31)), "0a".repeat(32)] {
            assert!(!is_digest(&bad), "{bad:?}");
        }
    }
}
