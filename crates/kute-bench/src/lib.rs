//! Evaluation and benchmarking (FR-016).
//!
//! Two entry points: [`evaluate`] measures one vocabulary on one dataset, and
//! [`suite::run`] runs the initial benchmark program (PRD section 17) on
//! datasets it builds itself.
//!
//! Rules this crate follows so the numbers can be trusted:
//!
//! * Compression is reported on complete serialized size, with and without the
//!   vocabulary file, next to gzip on the same bytes (FR-004).
//! * Round trips are checked on every document, not sampled.
//! * Every report names the hardware, software, configuration and dataset
//!   fingerprints it came from, and says which datasets are synthetic.
//! * Timings are medians of repeated runs on whatever machine ran them. They
//!   describe that machine, not KUTE in general.

pub mod data;
pub mod suite;

use flate2::{write::GzEncoder, Compression};
use kute::{Engine, Result, Tokenizer, Vocabulary};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Write;
use std::time::Instant;

/// Where a report was produced.
#[derive(Clone, Debug, Serialize)]
pub struct Environment {
    pub kute_version: String,
    pub rustc: String,
    pub build_profile: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub logical_cpus: usize,
}

impl Environment {
    pub fn detect() -> Self {
        Self {
            kute_version: kute::KUTE_VERSION.to_string(),
            rustc: env!("KUTE_BENCH_RUSTC").to_string(),
            build_profile: env!("KUTE_BENCH_PROFILE").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu: cpu_name(),
            logical_cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        }
    }
}

fn cpu_name() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(line) = text.lines().find(|l| l.starts_with("model name")) {
            if let Some((_, name)) = line.split_once(':') {
                return name.trim().to_string();
            }
        }
    }
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_string())
}

/// One named document of a dataset.
#[derive(Clone, Debug)]
pub struct Document {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Order-sensitive fingerprint of a dataset: names and contents.
pub fn fingerprint(documents: &[Document]) -> String {
    let mut h = Sha256::new();
    for d in documents {
        h.update((d.name.len() as u64).to_le_bytes());
        h.update(d.name.as_bytes());
        h.update((d.bytes.len() as u64).to_le_bytes());
        h.update(&d.bytes);
    }
    format!("sha256:{}", kute::hex(&h.finalize()))
}

pub fn gzip_len(bytes: &[u8]) -> usize {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).ok();
    encoder.finish().map(|v| v.len()).unwrap_or(0)
}

/// Measurements of one vocabulary on one dataset.
#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub vocabulary: String,
    pub vocabulary_digest: String,
    pub algorithm: String,
    pub adapter: Option<String>,
    pub dataset_fingerprint: String,
    pub documents: usize,
    /// Bytes as read from disk.
    pub raw_bytes: u64,
    /// Units the tokenizer saw after the adapter: bytes or symbols.
    pub canonical_units: u64,
    pub tokens: u64,
    /// Token density: canonical units per token.
    pub units_per_token: f64,
    pub raw_bytes_per_token: f64,
    pub vocabulary_size: usize,
    /// Share of the vocabulary this dataset actually used.
    pub vocabulary_utilization: f64,
    /// Size of all `.kute` containers: headers plus varint ids.
    pub serialized_bytes: u64,
    /// Size of the same sequences as compressed containers (format 2).
    pub compressed_bytes: u64,
    pub vocabulary_file_bytes: u64,
    /// raw / serialized. Above 1 means smaller than the input.
    pub ratio_serialized: f64,
    /// raw / compressed containers.
    pub ratio_compressed: f64,
    /// raw / (compressed containers + vocabulary file).
    pub ratio_compressed_with_vocabulary: f64,
    /// raw / (serialized + vocabulary file): what it costs to ship both.
    pub ratio_with_vocabulary: f64,
    /// gzip (default level) over each raw document, summed.
    pub gzip_bytes: u64,
    pub ratio_gzip: f64,
    /// decode(encode(x)) equals the adapter's canonical form, for every document.
    pub round_trip_canonical_exact: bool,
    /// decode(encode(x)) equals the original bytes, for every document.
    pub round_trip_raw_exact: bool,
    pub timing_iterations: usize,
    pub encode_seconds_median: f64,
    pub decode_seconds_median: f64,
    pub encode_mb_per_second: f64,
    pub decode_mb_per_second: f64,
    /// Time to encode one document, over every document in every timing pass.
    pub encode_latency: Latency,
    /// Time to decode one document, over the same.
    pub decode_latency: Latency,
    /// Threads available to the encoder. Text inputs of 1 MiB or more are
    /// encoded on all of them; smaller inputs use one.
    pub threads: usize,
    pub peak_memory: &'static str,
}

/// Spread of per-document times, in microseconds. With few samples the high
/// percentiles are the slowest sample or close to it; `samples` says how many.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Latency {
    pub samples: usize,
    pub p50_micros: f64,
    pub p90_micros: f64,
    pub p99_micros: f64,
    pub max_micros: f64,
}

impl Latency {
    /// Nearest-rank percentiles of `seconds`.
    pub fn of(mut seconds: Vec<f64>) -> Self {
        seconds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let at = |p: f64| -> f64 {
            if seconds.is_empty() {
                return 0.0;
            }
            let rank = (p * seconds.len() as f64).ceil() as usize;
            (seconds[rank.clamp(1, seconds.len()) - 1] * 1e7).round() / 10.0
        };
        Latency { samples: seconds.len(), p50_micros: at(0.50), p90_micros: at(0.90), p99_micros: at(0.99), max_micros: at(1.0) }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 { 0.0 } else { round(a as f64 / b as f64) }
}

fn round(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Evaluate `vocabulary` on `documents`. `iterations` timing passes are run
/// after one untimed pass that also checks every round trip.
pub fn evaluate(engine: &Engine, vocabulary: &Vocabulary, documents: &[Document], iterations: usize) -> Result<Evaluation> {
    let tokenizer = engine.tokenizer(vocabulary.clone())?;
    let tokenizer: &dyn Tokenizer = tokenizer.as_ref();
    let adapter = match &vocabulary.adapter {
        Some(spec) => Some((engine.registry().adapter(&spec.name)?, kute::AdapterConfig { options: spec.options.clone() })),
        None => None,
    };

    let mut raw_bytes = 0u64;
    let mut canonical_units = 0u64;
    let mut tokens = 0u64;
    let mut serialized = 0u64;
    let mut compressed = 0u64;
    let mut gzip = 0u64;
    let mut used: BTreeSet<u32> = BTreeSet::new();
    let (mut canonical_exact, mut raw_exact) = (true, true);
    let mut sequences = Vec::with_capacity(documents.len());
    for d in documents {
        let sequence = engine.encode_raw(tokenizer, &d.bytes)?;
        let decoded = engine.decode_raw(tokenizer, &sequence)?;
        raw_exact &= decoded == d.bytes;
        let canonical = match &adapter {
            Some((a, cfg)) => a.ingest(&d.bytes, cfg)?,
            None => kute::CanonicalInput::bytes(vocabulary.manifest.modality, d.bytes.clone()),
        };
        canonical_exact &= tokenizer.decode(&sequence)?.payload == canonical.payload;
        raw_bytes += d.bytes.len() as u64;
        canonical_units += canonical.payload.len() as u64;
        tokens += sequence.ids.len() as u64;
        serialized += sequence.to_bytes()?.len() as u64;
        let packed = sequence.to_bytes_compressed()?;
        canonical_exact &= kute::TokenSequence::from_bytes(&packed)? == sequence;
        compressed += packed.len() as u64;
        gzip += gzip_len(&d.bytes) as u64;
        used.extend(sequence.ids.iter().copied());
        sequences.push(sequence);
    }

    let iterations = iterations.max(1);
    let (mut encode_times, mut decode_times) = (Vec::new(), Vec::new());
    let (mut encode_each, mut decode_each) = (Vec::new(), Vec::new());
    for _ in 0..iterations {
        let start = Instant::now();
        let mut last = 0.0;
        for d in documents {
            std::hint::black_box(engine.encode_raw(tokenizer, &d.bytes)?);
            let now = start.elapsed().as_secs_f64();
            encode_each.push(now - last);
            last = now;
        }
        encode_times.push(start.elapsed().as_secs_f64());
        let start = Instant::now();
        let mut last = 0.0;
        for s in &sequences {
            std::hint::black_box(engine.decode_raw(tokenizer, s)?);
            let now = start.elapsed().as_secs_f64();
            decode_each.push(now - last);
            last = now;
        }
        decode_times.push(start.elapsed().as_secs_f64());
    }
    let (encode_s, decode_s) = (median(encode_times), median(decode_times));
    let mb = raw_bytes as f64 / 1_000_000.0;
    let vocabulary_file_bytes = kute::vocab::to_vec(vocabulary)?.len() as u64;

    Ok(Evaluation {
        vocabulary: format!("{} {}", vocabulary.id(), vocabulary.version()),
        vocabulary_digest: vocabulary.digest()?,
        algorithm: vocabulary.manifest.algorithm.clone(),
        adapter: vocabulary.adapter.as_ref().map(|a| a.name.clone()),
        dataset_fingerprint: fingerprint(documents),
        documents: documents.len(),
        raw_bytes,
        canonical_units,
        tokens,
        units_per_token: ratio(canonical_units, tokens),
        raw_bytes_per_token: ratio(raw_bytes, tokens),
        vocabulary_size: vocabulary.tokens.len(),
        vocabulary_utilization: round(used.len() as f64 / vocabulary.tokens.len().max(1) as f64),
        serialized_bytes: serialized,
        compressed_bytes: compressed,
        vocabulary_file_bytes,
        ratio_serialized: ratio(raw_bytes, serialized),
        ratio_compressed: ratio(raw_bytes, compressed),
        ratio_compressed_with_vocabulary: ratio(raw_bytes, compressed + vocabulary_file_bytes),
        ratio_with_vocabulary: ratio(raw_bytes, serialized + vocabulary_file_bytes),
        gzip_bytes: gzip,
        ratio_gzip: ratio(raw_bytes, gzip),
        round_trip_canonical_exact: canonical_exact,
        round_trip_raw_exact: raw_exact,
        timing_iterations: iterations,
        encode_seconds_median: encode_s,
        decode_seconds_median: decode_s,
        encode_mb_per_second: if encode_s > 0.0 { round(mb / encode_s) } else { 0.0 },
        decode_mb_per_second: if decode_s > 0.0 { round(mb / decode_s) } else { 0.0 },
        encode_latency: Latency::of(encode_each),
        decode_latency: Latency::of(decode_each),
        threads: rayon::current_num_threads(),
        peak_memory: "not measured",
    })
}

/// Read a dataset from a file or directory.
pub fn read_dataset(path: &std::path::Path) -> Result<Vec<Document>> {
    let mut out = Vec::new();
    for file in kute::collect_files(path)? {
        let name = file.strip_prefix(path).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        let name = if name.is_empty() { file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { name };
        out.push(Document { name, bytes: std::fs::read(&file)? });
    }
    if out.is_empty() {
        return Err(kute::KuteError::InvalidInput(format!("no files under {}", path.display())));
    }
    Ok(out)
}

/// Peak resident memory of this process so far, in bytes, where the
/// platform reports it (Linux and Windows). It is a high-water mark for the
/// whole process: measure one job per process.
#[cfg(target_os = "linux")]
pub fn peak_memory_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
}

#[cfg(windows)]
pub fn peak_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut counters = std::mem::MaybeUninit::<Counters>::zeroed();
    let size = std::mem::size_of::<Counters>() as u32;
    // SAFETY: the structure matches PROCESS_MEMORY_COUNTERS, is zeroed, and
    // its size is passed; the call only writes within it.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), size) };
    if ok == 0 {
        return None;
    }
    // SAFETY: the call succeeded and filled the structure.
    Some(unsafe { counters.assume_init() }.peak_working_set_size as u64)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn peak_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::Latency;

    #[test]
    fn latency_percentiles_are_nearest_rank() {
        let l = Latency::of((1..=100).rev().map(|n| n as f64 * 1e-6).collect());
        assert_eq!((l.samples, l.p50_micros, l.p90_micros, l.p99_micros, l.max_micros), (100, 50.0, 90.0, 99.0, 100.0));
        let one = Latency::of(vec![2e-6]);
        assert_eq!((one.p50_micros, one.p99_micros, one.max_micros), (2.0, 2.0, 2.0));
        assert_eq!(Latency::of(Vec::new()).max_micros, 0.0);
    }
}
