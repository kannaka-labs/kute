//! `kute job`: give a training job to a `kute serve --jobs-dir`, wait for
//! it, and take the vocabulary it made.
//!
//! The other side is believed about nothing that can be checked here.
//! The file is read as a vocabulary, its digest is computed from its
//! content and must be the one the service named, and its own test
//! vectors must pass, before anything is written. What cannot be
//! checked is that the service trained on what it was sent: that would
//! take training again. The report says so.

use crate::registry;
use kute::{vocab, Engine, KuteError};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What `kute job` was asked.
pub struct Ask {
    pub at: String,
    pub input: Vec<PathBuf>,
    pub id: String,
    pub vocab_version: String,
    pub algorithm: String,
    pub adapter: String,
    pub adapter_options: Vec<String>,
    pub vocab_size: u32,
    pub min_frequency: u64,
    pub output: PathBuf,
    pub force: bool,
    pub wait: Duration,
    pub max_bytes: u64,
}

/// One request may take this long, whatever the wait for the job is.
const EXCHANGE: Duration = Duration::from_secs(20);
/// The largest answer that is not the vocabulary: a job, or a refusal.
const MAX_ANSWER_BYTES: u64 = 1 << 20;

/// The client under this is the registry's; its errors name a registry.
fn ours(e: KuteError) -> KuteError {
    match e {
        KuteError::Backend(text) => KuteError::Backend(text.replacen("registry: ", "service: ", 1)),
        KuteError::LimitExceeded(text) => KuteError::LimitExceeded(text.replacen("the registry's answer", "the service's answer", 1)),
        KuteError::InvalidConfig(text) => KuteError::InvalidConfig(text.replacen("registry address", "service address", 1)),
        other => other,
    }
}

/// What the other side said, kept short and free of control characters.
fn said(status: u16, body: &[u8]) -> String {
    let why = serde_json::from_slice::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(|m| m.chars().filter(|c| !c.is_control()).take(300).collect::<String>()));
    format!("answered {status}{}", why.map_or(String::new(), |m| format!(": {m}")))
}

fn failed(what: impl Into<String>) -> KuteError {
    KuteError::Backend(format!("service: {}", what.into()))
}

struct Service {
    authority: String,
    base: String,
}

impl Service {
    fn ask(&self, method: &str, path: &str, body: Option<&[u8]>, max_bytes: u64) -> kute::Result<(u16, Vec<u8>)> {
        registry::request(method, &self.authority, &format!("{}{path}", self.base), body, max_bytes, EXCHANGE, false).map_err(ours)
    }

    fn job(&self, id: &str) -> kute::Result<Value> {
        let (status, body) = self.ask("GET", &format!("/v1/jobs/{id}"), None, MAX_ANSWER_BYTES)?;
        if status != 200 {
            return Err(failed(format!("asked about {id}, {}", said(status, &body))));
        }
        serde_json::from_slice(&body).map_err(|_| failed("the answer about the job is not JSON"))
    }
}

/// A job's name is put in a path, and it comes from the other side.
fn is_job_id(text: &str) -> bool {
    text.strip_prefix("job-").is_some_and(|n| !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()))
}

pub fn run(engine: &Engine, ask: Ask) -> kute::Result<Value> {
    // This side's mistakes first, before the service is given work.
    let (authority, base) = registry::parse_url(&ask.at).map_err(ours)?;
    if ask.output.exists() && !ask.force {
        return Err(KuteError::InvalidConfig(format!("{} exists; pass --force to replace it", ask.output.display())));
    }
    let mut options = serde_json::Map::new();
    for option in &ask.adapter_options {
        let (key, value) = option.split_once('=').ok_or_else(|| KuteError::InvalidConfig(format!("adapter option {option:?}: expected key=value")))?;
        options.insert(key.to_string(), json!(value));
    }
    let mut texts = Vec::with_capacity(ask.input.len());
    for path in &ask.input {
        let bytes = std::fs::read(path).map_err(|e| KuteError::InvalidConfig(format!("{}: {e}", path.display())))?;
        texts.push(String::from_utf8(bytes).map_err(|_| KuteError::InvalidConfig(format!("{} is not UTF-8 text; a job's corpus travels as JSON strings, so train this one with `kute train`", path.display())))?);
    }
    let corpus_bytes: usize = texts.iter().map(String::len).sum();
    let body = json!({
        "texts": texts,
        "vocabulary_id": ask.id,
        "version": ask.vocab_version,
        "vocab_size": ask.vocab_size,
        "algorithm": ask.algorithm,
        "adapter": ask.adapter,
        "adapter_options": options,
        "min_frequency": ask.min_frequency,
    })
    .to_string();
    drop(texts);

    let service = Service { authority, base };
    // What the service says it takes, asked first: a body larger than it
    // reads is refused by a closed connection, which says nothing.
    let (status, answer) = service.ask("GET", "/v1/capabilities", None, MAX_ANSWER_BYTES)?;
    if status != 200 {
        return Err(failed(format!("asked what it takes, {}", said(status, &answer))));
    }
    let takes: Value = serde_json::from_slice(&answer).map_err(|_| failed("what it says it takes is not JSON"))?;
    if !takes["jobs"].is_object() {
        return Err(failed("it takes no jobs; its operator starts it with --jobs-dir DIR to take them"));
    }
    if let Some(most) = takes["limits"]["max_body_bytes"].as_u64() {
        if body.len() as u64 > most {
            return Err(KuteError::LimitExceeded(format!("the corpus makes a request of {} bytes and the service reads at most {most}; train it with `kute train`, or ask the operator for a larger --max-input-bytes", body.len())));
        }
    }
    let (status, answer) = service.ask("POST", "/v1/jobs", Some(body.as_bytes()), MAX_ANSWER_BYTES)?;
    drop(body);
    if status != 202 {
        return Err(failed(format!("the job was not taken: {}", said(status, &answer))));
    }
    let taken: Value = serde_json::from_slice(&answer).map_err(|_| failed("the answer to the job is not JSON"))?;
    let id = taken["id"].as_str().filter(|id| is_job_id(id)).ok_or_else(|| failed("the answer to the job does not name it"))?.to_string();
    eprintln!("{} took the job as {id}", ask.at);

    let started = Instant::now();
    let mut pause = Duration::from_millis(100);
    let job = loop {
        let job = service.job(&id)?;
        if job["finished_unix"].is_u64() {
            break job;
        }
        if started.elapsed() >= ask.wait {
            // Out of time. A job still in the line is taken out of it,
            // so that it does not train for nobody; one that has started
            // cannot be stopped, and the caller is told where it is.
            let (status, answer) = service.ask("DELETE", &format!("/v1/jobs/{id}"), None, MAX_ANSWER_BYTES)?;
            return Err(failed(if status == 200 {
                format!("{id} had not started after {} s and was withdrawn; nothing was trained", ask.wait.as_secs())
            } else {
                format!("{id} had not finished after {} s. It was not withdrawn ({}), so it may still end on the service: GET {}/v1/jobs/{id}", ask.wait.as_secs(), said(status, &answer), ask.at.trim_end_matches('/'))
            }));
        }
        std::thread::sleep(pause.min(ask.wait.saturating_sub(started.elapsed())).max(Duration::from_millis(20)));
        pause = (pause * 2).min(Duration::from_secs(2));
    };
    match job["state"].as_str() {
        Some("done") => {}
        Some("failed") => return Err(failed(format!("{id} failed: {}", job["error"].as_str().unwrap_or("no reason given").chars().filter(|c| !c.is_control()).take(300).collect::<String>()))),
        Some("withdrawn") => return Err(failed(format!("{id} was withdrawn by another caller before it started"))),
        _ => return Err(failed(format!("{id} finished in a state this client does not know"))),
    }
    let named = job["result"]["digest"].as_str().filter(|d| registry::is_digest(d)).ok_or_else(|| failed(format!("{id} is done and names no digest")))?.to_string();

    let (status, file) = service.ask("GET", &format!("/v1/jobs/{id}/vocabulary"), None, ask.max_bytes)?;
    if status != 200 {
        return Err(failed(format!("asked for the vocabulary of {id}, {}", said(status, &file[..file.len().min(MAX_ANSWER_BYTES as usize)]))));
    }
    let vocabulary = vocab::from_slice(&file)?;
    vocabulary.validate()?;
    let arrived = vocabulary.compute_digest()?;
    if arrived != named {
        return Err(KuteError::Integrity(format!("the service named {named} and sent a vocabulary whose content has digest {arrived}; nothing was written")));
    }
    if vocabulary.id().as_str() != ask.id {
        return Err(KuteError::Integrity(format!("the service sent a vocabulary named {}, not {}; nothing was written", vocabulary.id(), ask.id)));
    }
    let report = engine.verify(&vocabulary)?;
    if !report.ok() {
        return Err(KuteError::Integrity(format!("the vocabulary fails its own test vectors: {}", report.failures.join("; "))));
    }
    vocab::save(&vocabulary, &ask.output, ask.force)?;
    Ok(json!({
        "service": ask.at,
        "job": id,
        "corpus_bytes_sent": corpus_bytes,
        "digest": arrived,
        "digest_recomputed_from_content": true,
        "id": vocabulary.id().as_str(),
        "version": vocabulary.version().to_string(),
        "tokens": vocabulary.len(),
        "bytes_received": file.len(),
        "test_vectors_pass": true,
        "service_says": { "train_seconds": job["result"]["train_seconds"], "file": job["result"]["file"] },
        "output": ask.output,
        "means": "the file is a valid vocabulary with the name asked for, and its content is what the digest names. That the service trained it on the corpus sent, with the settings sent, is its word: `kute train` on the same input gives the digest to compare.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_name_from_the_other_side_is_read_strictly() {
        assert!(is_job_id("job-1") && is_job_id("job-18446744073709551615"));
        for bad in ["", "job-", "job-1/../x", "job-1?x", "job--1", "job-1 ", "JOB-1", "job-123456789012345678901", "1"] {
            assert!(!is_job_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn errors_from_the_shared_client_name_the_service() {
        assert_eq!(ours(KuteError::Backend("registry: no complete answer in time".into())).to_string(), KuteError::Backend("service: no complete answer in time".into()).to_string());
    }
}
