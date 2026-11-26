//! Training jobs for the HTTP service.
//!
//! Off unless the operator names a directory for the results. A caller
//! sends a corpus and what to train; the answer is a job, which waits its
//! turn, runs, and ends with a vocabulary file in that directory, named by
//! its digest. One job runs at a time. A job cannot be stopped once it
//! runs, so what a job may cost is bounded when it is accepted: the corpus
//! by the service's body limit, the vocabulary by a size limit, the line
//! by a queue length, and the directory by a size past which no job is
//! taken and no file is written.
//!
//! The service does not start using a vocabulary it trained: it is not
//! added to what encode and decode can name, and it is not given by the
//! registry. The operator takes the file and decides.

use kute::{vocab, AdapterConfig, Engine, KuteError, TrainingConfig, Version, VocabularyId};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// What accepting jobs may cost the service.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Jobs that may wait behind the one running. Past it a job is
    /// refused. Each waiting job holds its corpus in memory.
    pub queue: usize,
    /// Finished jobs remembered. Past it the oldest finished one is
    /// forgotten; its file stays.
    pub kept: usize,
    /// Largest vocabulary a job may ask for.
    pub max_vocab_size: u32,
    /// Bytes of files the directory may hold. At or past it no job is
    /// taken, and a job that would write past it fails instead.
    pub max_dir_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { queue: 4, kept: 64, max_vocab_size: 65_536, max_dir_bytes: 1 << 30 }
    }
}

struct Work {
    algorithm: String,
    adapter: String,
    adapter_config: AdapterConfig,
    config: TrainingConfig,
    documents: Vec<Vec<u8>>,
}

struct Job {
    id: String,
    /// What was asked, without the corpus.
    asked: Value,
    state: &'static str,
    submitted: u64,
    started: Option<u64>,
    finished: Option<u64>,
    result: Option<Value>,
    error: Option<String>,
    /// The file, for a job that is done.
    file: Option<PathBuf>,
}

impl Job {
    fn describe(&self) -> Value {
        json!({
            "id": self.id,
            "state": self.state,
            "asked": self.asked,
            "submitted_unix": self.submitted,
            "started_unix": self.started,
            "finished_unix": self.finished,
            "result": self.result,
            "error": self.error,
        })
    }
}

#[derive(Default)]
struct State {
    next: u64,
    jobs: Vec<Job>,
    waiting: VecDeque<(String, Work)>,
    /// The size of the last vocabulary there was no room to write, or 0.
    /// While less room than this is left, no job is taken: it would
    /// train in full and then fail at the write.
    did_not_fit: u64,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").field("dir", &self.dir).field("limits", &self.limits).finish_non_exhaustive()
    }
}

pub struct Jobs {
    dir: PathBuf,
    limits: Limits,
    /// What the engine can train with, to refuse other names at the door.
    algorithms: Vec<String>,
    adapters: Vec<String>,
    state: Mutex<State>,
    wake: Condvar,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Why a job was not taken.
pub enum Refusal {
    /// The request is wrong; say why.
    Bad(String),
    /// The line is full; try later.
    Full(String),
    /// The directory holds as much as it may; the operator must make room.
    NoRoom(String),
}

impl Jobs {
    /// Make the directory if it is not there, and start the one thread
    /// that runs jobs.
    pub fn start(dir: PathBuf, limits: Limits) -> kute::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let dir = dir.canonicalize()?;
        let engine = Engine::new();
        let jobs = Arc::new(Jobs { dir, limits, algorithms: engine.registry().algorithm_names(), adapters: engine.registry().adapter_names(), state: Mutex::new(State::default()), wake: Condvar::new() });
        let runner = jobs.clone();
        std::thread::Builder::new().name("kute-jobs".into()).spawn(move || runner.run()).map_err(KuteError::Io)?;
        Ok(jobs)
    }

    /// A panic while the lock was held leaves a list that is still a
    /// list; the service goes on.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn limits(&self) -> Value {
        json!({ "waiting_at_most": self.limits.queue, "finished_remembered": self.limits.kept, "max_vocab_size": self.limits.max_vocab_size, "max_dir_bytes": self.limits.max_dir_bytes, "running_at_once": 1 })
    }

    /// Files in the directory and their bytes: everything there, whoever
    /// put it there. Directories inside it are not entered.
    fn held(&self) -> std::io::Result<(u64, u64)> {
        let (mut files, mut bytes) = (0u64, 0u64);
        for entry in std::fs::read_dir(&self.dir)? {
            let meta = entry?.metadata()?;
            if meta.is_file() {
                files += 1;
                bytes = bytes.saturating_add(meta.len());
            }
        }
        Ok((files, bytes))
    }

    fn directory(&self) -> Value {
        match self.held() {
            Ok((files, bytes)) => json!({ "files": files, "bytes": bytes, "max_bytes": self.limits.max_dir_bytes }),
            Err(e) => json!({ "error": format!("the jobs directory cannot be read: {e}"), "max_bytes": self.limits.max_dir_bytes }),
        }
    }

    /// Check a request and put it in line.
    pub fn submit(&self, args: &Value) -> Result<Value, Refusal> {
        let bad = |why: String| Refusal::Bad(why);
        let object = args.as_object().ok_or_else(|| bad("the body must be a JSON object".into()))?;
        const KNOWN: [&str; 9] = ["operation", "texts", "vocabulary_id", "vocab_size", "version", "algorithm", "adapter", "adapter_options", "min_frequency"];
        if let Some(unknown) = object.keys().find(|k| !KNOWN.contains(&k.as_str())) {
            return Err(bad(format!("unknown field {unknown:?}; a job takes {}", KNOWN.join(", "))));
        }
        let text = |name: &str, default: Option<&str>| -> Result<String, Refusal> {
            match (object.get(name), default) {
                (Some(Value::String(s)), _) => Ok(s.clone()),
                (None | Some(Value::Null), Some(d)) => Ok(d.to_string()),
                (None | Some(Value::Null), None) => Err(Refusal::Bad(format!("{name} is required"))),
                _ => Err(Refusal::Bad(format!("{name} must be a string"))),
            }
        };
        if text("operation", Some("train"))? != "train" {
            return Err(bad("the only job is train".into()));
        }
        let texts = object.get("texts").and_then(Value::as_array).ok_or_else(|| bad("texts is required: a list of strings, the corpus".into()))?;
        if texts.is_empty() {
            return Err(bad("texts is empty".into()));
        }
        let mut documents = Vec::with_capacity(texts.len());
        for (i, t) in texts.iter().enumerate() {
            documents.push(t.as_str().ok_or_else(|| bad(format!("texts[{i}] is not a string")))?.as_bytes().to_vec());
        }
        let vocab_size = object.get("vocab_size").and_then(Value::as_u64).ok_or_else(|| bad("vocab_size is required: a whole number".into()))?;
        if vocab_size > u64::from(self.limits.max_vocab_size) {
            return Err(bad(format!("vocab_size {vocab_size} is more than this service trains: {}", self.limits.max_vocab_size)));
        }
        let algorithm = text("algorithm", Some("bpe"))?;
        let adapter = text("adapter", Some("text"))?;
        // What training would refuse for certain is refused here, so it
        // does not take a place in the line.
        if algorithm == kute::algorithms::DICTIONARY {
            return Err(bad("the dictionary algorithm needs entries, which a job cannot give".into()));
        }
        if !self.algorithms.contains(&algorithm) {
            return Err(bad(format!("unknown algorithm {algorithm:?}; this service has {}", self.algorithms.join(", "))));
        }
        if !self.adapters.contains(&adapter) {
            return Err(bad(format!("unknown adapter {adapter:?}; this service has {}", self.adapters.join(", "))));
        }
        if algorithm == "bpe" && vocab_size < 256 {
            return Err(bad(format!("vocab_size {vocab_size} is less than bpe can train: the 256 bytes come first")));
        }
        let version = text("version", Some("0.1.0"))?;
        let id = text("vocabulary_id", None)?;
        let fail = |e: KuteError| Refusal::Bad(e.to_string());
        let mut config = TrainingConfig::new(VocabularyId::parse(&id).map_err(fail)?, vocab_size as u32);
        config.vocabulary_version = Version::parse(&version).map_err(|e| bad(format!("version {version:?}: {e}")))?;
        config.pretokenizer = if algorithm == "sequence-bpe" { kute::Pretokenizer::None } else { crate::parse_pretokenizer(None, &adapter).map_err(fail)? };
        if let Some(v) = object.get("min_frequency") {
            config.min_frequency = v.as_u64().ok_or_else(|| bad("min_frequency must be a whole number".into()))?;
        }
        let mut adapter_config = AdapterConfig::default();
        match object.get("adapter_options") {
            None | Some(Value::Null) => {}
            Some(Value::Object(options)) => {
                for (key, value) in options {
                    adapter_config = adapter_config.with(key, value.as_str().ok_or_else(|| bad(format!("adapter option {key:?} must be a string")))?);
                }
            }
            Some(_) => return Err(bad("adapter_options must be an object of strings".into())),
        }
        config.validate().map_err(fail)?;
        let corpus_bytes: usize = documents.iter().map(Vec::len).sum();
        let asked = json!({
            "operation": "train",
            "vocabulary_id": id,
            "version": version,
            "vocab_size": vocab_size,
            "algorithm": algorithm,
            "adapter": adapter,
            "adapter_options": adapter_config.options,
            "min_frequency": config.min_frequency,
            "documents": documents.len(),
            "corpus_bytes": corpus_bytes,
        });

        match self.held() {
            Ok((_, bytes)) if bytes >= self.limits.max_dir_bytes => {
                return Err(Refusal::NoRoom(format!("the jobs directory holds {bytes} bytes and may hold {}; no job is taken until its operator makes room", self.limits.max_dir_bytes)));
            }
            Ok((_, bytes)) => {
                let did_not_fit = self.lock().did_not_fit;
                if self.limits.max_dir_bytes - bytes < did_not_fit {
                    return Err(Refusal::NoRoom(format!("the jobs directory holds {bytes} bytes and may hold {}; the last vocabulary trained was {did_not_fit} bytes and did not fit, so no job is taken until its operator makes room", self.limits.max_dir_bytes)));
                }
            }
            Err(e) => return Err(Refusal::NoRoom(format!("the jobs directory cannot be read: {e}"))),
        }
        let mut state = self.lock();
        if state.waiting.len() >= self.limits.queue {
            return Err(Refusal::Full(format!("{} jobs are waiting already; this service lines up no more", state.waiting.len())));
        }
        state.next += 1;
        let id = format!("job-{}", state.next);
        let job = Job { id: id.clone(), asked, state: "waiting", submitted: now(), started: None, finished: None, result: None, error: None, file: None };
        let described = job.describe();
        state.jobs.push(job);
        state.waiting.push_back((id, Work { algorithm, adapter, adapter_config, config, documents }));
        // Forget the oldest finished jobs past what is remembered.
        while state.jobs.iter().filter(|j| j.finished.is_some()).count() > self.limits.kept {
            if let Some(at) = state.jobs.iter().position(|j| j.finished.is_some()) {
                state.jobs.remove(at);
            }
        }
        drop(state);
        self.wake.notify_one();
        Ok(described)
    }

    pub fn list(&self) -> Value {
        let state = self.lock();
        json!({ "jobs": state.jobs.iter().map(Job::describe).collect::<Vec<_>>(), "limits": self.limits(), "directory": self.directory() })
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        self.lock().jobs.iter().find(|j| j.id == id).map(Job::describe)
    }

    /// The vocabulary file of a job that is done. `None` when there is no
    /// such job; `Err` says why a job that exists has no file to give.
    pub fn file(&self, id: &str) -> Option<Result<Vec<u8>, String>> {
        let path = {
            let state = self.lock();
            let job = state.jobs.iter().find(|j| j.id == id)?;
            match &job.file {
                Some(path) => path.clone(),
                None => return Some(Err(format!("the job is {}; it has no vocabulary", job.state))),
            }
        };
        Some(std::fs::read(&path).map_err(|_| "the file is no longer in the jobs directory".to_string()))
    }

    fn run(&self) {
        let engine = Engine::new();
        loop {
            let (id, work) = {
                let mut state = self.lock();
                loop {
                    if let Some(next) = state.waiting.pop_front() {
                        if let Some(job) = state.jobs.iter_mut().find(|j| j.id == next.0) {
                            job.state = "running";
                            job.started = Some(now());
                        }
                        break next;
                    }
                    state = self.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            };
            let started = std::time::Instant::now();
            // A panic in training must not end the thread: the jobs
            // behind this one would wait for ever.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.train(&engine, work)));
            let seconds = started.elapsed().as_secs_f64();
            let mut state = self.lock();
            let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) else { continue };
            job.finished = Some(now());
            match outcome {
                Ok(Ok((mut result, path))) => {
                    result["train_seconds"] = json!(seconds);
                    job.state = "done";
                    job.result = Some(result);
                    job.file = Some(path);
                }
                Ok(Err(e)) => {
                    job.state = "failed";
                    job.error = Some(e.to_string());
                }
                Err(_) => {
                    job.state = "failed";
                    job.error = Some("training failed unexpectedly".into());
                }
            }
        }
    }

    fn train(&self, engine: &Engine, work: Work) -> kute::Result<(Value, PathBuf)> {
        let vocabulary = engine.train(&work.algorithm, &work.adapter, &work.adapter_config, &work.documents, &work.config)?;
        let digest = vocabulary.digest()?;
        // The name is the digest and nothing a caller chose, so a job
        // cannot write anywhere but a file of this form in the directory.
        let hex = digest.strip_prefix("sha256:").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| KuteError::Integrity("the digest is not a SHA-256 digest".into()))?;
        let name = format!("{hex}.kvf");
        let path = self.dir.join(&name);
        let bytes = vocab::to_vec(&vocabulary)?;
        // Jobs taken while there was room may together pass the limit;
        // the one that would is not written. A file already there under
        // this name is replaced by the same bytes and adds nothing.
        if !path.exists() {
            let (_, held) = self.held()?;
            if held.saturating_add(bytes.len() as u64) > self.limits.max_dir_bytes {
                // Remembered only when making room could help. A
                // vocabulary larger than the whole limit fits no
                // directory, even an empty one: remembering it would
                // refuse every later job, however small, until a restart.
                if bytes.len() as u64 <= self.limits.max_dir_bytes {
                    self.lock().did_not_fit = bytes.len() as u64;
                }
                return Err(KuteError::LimitExceeded(format!("the vocabulary is {} bytes; the jobs directory holds {held} and may hold {}", bytes.len(), self.limits.max_dir_bytes)));
            }
        }
        // Two jobs that give the same vocabulary give the same file.
        let writing = self.dir.join(format!("{name}.writing-{}", std::process::id()));
        std::fs::write(&writing, &bytes)?;
        std::fs::rename(&writing, &path)?;
        Ok((json!({ "digest": digest, "file": name, "file_bytes": bytes.len(), "summary": vocab::summarize(&vocabulary)? }), path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_that_went_away_refuses_jobs_and_one_that_came_back_takes_them() {
        let dir = std::env::temp_dir().join(format!("kute-jobs-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let jobs = Jobs::start(dir.clone(), Limits::default()).unwrap();
        let job = json!({ "texts": ["the engine reads the log ".repeat(20)], "vocabulary_id": "test:gone", "vocab_size": 260 });
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(jobs.submit(&job), Err(Refusal::NoRoom(why)) if why.contains("cannot be read")));
        assert!(jobs.list()["directory"]["error"].is_string());
        assert_eq!(jobs.list()["jobs"].as_array().unwrap().len(), 0);
        // The operator puts it back: the same service takes jobs again.
        std::fs::create_dir_all(&dir).unwrap();
        let taken = jobs.submit(&job).ok().expect("taken");
        let id = taken["id"].as_str().unwrap();
        let started = std::time::Instant::now();
        let done = loop {
            let job = jobs.get(id).unwrap();
            if job["finished_unix"].is_u64() {
                break job;
            }
            assert!(started.elapsed().as_secs() < 120, "{job}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(done["state"], "done", "{done}");
        assert_eq!(jobs.list()["directory"]["files"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
