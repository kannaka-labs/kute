//! The initial benchmark program (PRD section 17), cases B01 to B05, plus the
//! engineering targets from PRD 11.1 that can be measured locally.
//!
//! Cases B06 to B10 (DNA, telemetry, mathematics, quantum circuits, HRM
//! states) need adapters and datasets planned for later releases and are
//! listed in the report as not run.

use crate::{data, evaluate, fingerprint, Document, Environment, Evaluation};
use kute::{AdapterConfig, Engine, Pretokenizer, Result, TrainingConfig, Vocabulary, VocabularyId};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Options {
    /// Repository root, for the in-repo prose and source corpora (B01, B02).
    pub repo_root: Option<PathBuf>,
    pub iterations: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct DatasetInfo {
    pub description: String,
    pub synthetic: bool,
    pub train_fingerprint: String,
    pub train_documents: usize,
    pub train_bytes: u64,
    pub eval_fingerprint: String,
    pub eval_documents: usize,
    pub eval_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaseConfig {
    pub algorithm: String,
    pub adapter: String,
    pub adapter_options: std::collections::BTreeMap<String, String>,
    pub pretokenizer: Option<String>,
    pub requested_vocab_size: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub title: String,
    pub dataset: DatasetInfo,
    pub config: CaseConfig,
    pub train_seconds: f64,
    /// The trained vocabulary on the held-out evaluation set.
    pub trained: Evaluation,
    /// The same evaluation set through the one-token-per-unit baseline.
    pub baseline_name: String,
    pub baseline: Evaluation,
    /// baseline tokens / trained tokens on the evaluation set.
    pub token_reduction_vs_baseline: f64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetCheck {
    pub target: String,
    pub prd_value: String,
    pub measured: String,
    pub met: Option<bool>,
    pub method: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendComparison {
    pub case: String,
    pub native_train_seconds: f64,
    pub rustbpe_train_seconds: f64,
    pub merges_identical: bool,
    pub eval_encodings_identical: bool,
    pub native_encode_mb_per_second: f64,
    pub rustbpe_encode_mb_per_second: f64,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SuiteReport {
    pub report: &'static str,
    pub environment: Environment,
    pub methodology: Vec<&'static str>,
    pub cases: Vec<CaseResult>,
    pub skipped: Vec<String>,
    pub targets: Vec<TargetCheck>,
    /// Present only when built with the `rustbpe` feature.
    pub rustbpe_comparison: Vec<BackendComparison>,
    pub rustbpe_comparison_status: String,
}

struct Case {
    id: &'static str,
    title: &'static str,
    description: String,
    synthetic: bool,
    algorithm: &'static str,
    baseline: &'static str,
    adapter: &'static str,
    adapter_config: AdapterConfig,
    pretokenizer: Pretokenizer,
    vocab_size: u32,
    train: Vec<Document>,
    eval: Vec<Document>,
    notes: Vec<String>,
}

fn doc(name: &str, bytes: Vec<u8>) -> Vec<Document> {
    vec![Document { name: name.to_string(), bytes }]
}

fn total(docs: &[Document]) -> u64 {
    docs.iter().map(|d| d.bytes.len() as u64).sum()
}

fn read_tree(root: &Path, dirs: &[&str], extension: &str) -> Vec<Document> {
    let mut out = Vec::new();
    for dir in dirs {
        let base = root.join(dir);
        let Ok(files) = kute::collect_files(&base) else { continue };
        for file in files {
            if file.extension().and_then(|e| e.to_str()) != Some(extension) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&file) {
                // Normalize line endings so a Windows checkout and a Linux
                // checkout produce the same fingerprint.
                let bytes = String::from_utf8(bytes).map(|s| s.replace("\r\n", "\n").into_bytes()).unwrap_or_else(|e| e.into_bytes());
                let name = file.strip_prefix(root).unwrap_or(&file).to_string_lossy().replace('\\', "/");
                out.push(Document { name, bytes });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn cases(options: &Options, skipped: &mut Vec<String>) -> Vec<Case> {
    let mut out = Vec::new();
    match &options.repo_root {
        Some(root) => {
            let train = read_tree(root, &["docs", "specifications"], "md");
            let eval = read_tree(root, &["README.md", "CONTRIBUTING.md"], "md");
            if train.is_empty() || eval.is_empty() {
                skipped.push("KUTE-B01 natural language: no markdown found under the repository root".into());
            } else {
                out.push(Case {
                    id: "KUTE-B01",
                    title: "Natural language: density and throughput",
                    description: "English technical prose from this repository. Train: docs/ and specifications/. Evaluate: README.md and CONTRIBUTING.md. Small and from one author, so not representative of general English.".into(),
                    synthetic: false,
                    algorithm: "bpe",
                    baseline: "bytes",
                    adapter: "text",
                    adapter_config: AdapterConfig::default(),
                    pretokenizer: Pretokenizer::Gpt4,
                    vocab_size: 2048,
                    train,
                    eval,
                    notes: vec!["The corpus is tens of kilobytes. A 2048-token vocabulary costs more to ship than this dataset saves; see ratio_with_vocabulary.".into()],
                });
            }
            let train = read_tree(root, &["crates/kute-types", "crates/kute-core", "crates/kute-bpe", "crates/kute-algorithms", "crates/kute-adapters", "crates/kute-vocab"], "rs");
            let eval = read_tree(root, &["crates/kute", "crates/kute-cli", "crates/kute-bench", "plugins/hrm"], "rs");
            if train.is_empty() || eval.is_empty() {
                skipped.push("KUTE-B02 source code: no Rust sources found under the repository root".into());
            } else {
                out.push(Case {
                    id: "KUTE-B02",
                    title: "Source code: byte-exact structure preservation",
                    description: "Rust source from this repository. Train: the six library crates below the engine. Evaluate: the engine, CLI, benchmark and HRM crates.".into(),
                    synthetic: false,
                    algorithm: "bpe",
                    baseline: "bytes",
                    adapter: "text",
                    adapter_config: AdapterConfig::default(),
                    pretokenizer: Pretokenizer::Gpt4,
                    vocab_size: 4096,
                    train,
                    eval,
                    notes: vec!["Structure preservation is measured as exact reconstruction of every file, including indentation and line ends. AST-aware tokenization is a later release.".into()],
                });
            }
        }
        None => {
            skipped.push("KUTE-B01 natural language: needs --repo-root".into());
            skipped.push("KUTE-B02 source code: needs --repo-root".into());
        }
    }
    out.push(Case {
        id: "KUTE-B03",
        title: "Raw binary: exact reconstruction",
        description: "Fixed-layout 32-byte binary records from a seeded generator (train seed 31, 6000 records; evaluate seed 32, 2000 records).".into(),
        synthetic: true,
        algorithm: "bpe",
        baseline: "bytes",
        adapter: "binary",
        adapter_config: AdapterConfig::default(),
        pretokenizer: Pretokenizer::None,
        vocab_size: 1024,
        train: doc("records-train.bin", data::binary_records(31, 6000)),
        eval: doc("records-eval.bin", data::binary_records(32, 2000)),
        notes: vec!["Synthetic and highly regular.".into()],
    });
    out.push(Case {
        id: "KUTE-B03-control",
        title: "Raw binary control: uniform random bytes",
        description: "Uniform random bytes from a seeded generator (train seed 41, 128 KiB; evaluate seed 42, 64 KiB). Incompressible by construction.".into(),
        synthetic: true,
        algorithm: "bpe",
        baseline: "bytes",
        adapter: "binary",
        adapter_config: AdapterConfig::default(),
        pretokenizer: Pretokenizer::None,
        vocab_size: 1024,
        train: doc("random-train.bin", data::random_bytes(41, 128 * 1024)),
        eval: doc("random-eval.bin", data::random_bytes(42, 64 * 1024)),
        notes: vec!["Negative control. The correct result is no size reduction (ratio_serialized at or below 1) with exact reconstruction.".into()],
    });
    out.push(Case {
        id: "KUTE-B04",
        title: "Industrial alarms: recurring event sequences",
        description: "A plant alarm log as JSONL from a seeded generator built on five episode templates plus noise events (train seed 51, 4000 episodes; evaluate seed 52, 1000 episodes).".into(),
        synthetic: true,
        algorithm: "sequence-bpe",
        baseline: "dictionary",
        adapter: "jsonl-events",
        adapter_config: AdapterConfig::default().with("field", "tag"),
        pretokenizer: Pretokenizer::None,
        vocab_size: 64,
        train: doc("alarms-train.jsonl", data::alarm_log(51, 4000)),
        eval: doc("alarms-eval.jsonl", data::alarm_log(52, 1000)),
        notes: vec![
            "The adapter keeps only the event tag; timestamps and unit names are dropped, so round_trip_raw_exact is false by design and the size ratios compare a tag stream against full records.".into(),
            "Synthetic: the episodes are the generator's own templates, so finding them is the expected result.".into(),
        ],
    });
    out.push(Case {
        id: "KUTE-B05",
        title: "Agent messages: communication efficiency",
        description: "Agent-to-agent JSONL messages with a fixed envelope from a seeded generator (train seed 61, 4000 messages; evaluate seed 62, 1000 messages).".into(),
        synthetic: true,
        algorithm: "bpe",
        baseline: "bytes",
        adapter: "jsonl",
        adapter_config: AdapterConfig::default(),
        pretokenizer: Pretokenizer::None,
        vocab_size: 2048,
        train: doc("messages-train.jsonl", data::agent_messages(61, 4000)),
        eval: doc("messages-eval.jsonl", data::agent_messages(62, 1000)),
        notes: vec![
            "The jsonl adapter canonicalizes each record, so round_trip_raw_exact is false and round_trip_canonical_exact is the relevant check.".into(),
            "Synthetic and template-built, so far more repetitive than real agent traffic.".into(),
        ],
    });
    out
}

fn config_for(case: &Case, id_suffix: &str) -> Result<TrainingConfig> {
    let id = format!("kute:bench:{}{}", case.id.to_lowercase(), id_suffix);
    let mut config = TrainingConfig::new(VocabularyId::parse(&id)?, case.vocab_size);
    config.pretokenizer = case.pretokenizer.clone();
    config.description = Some(case.title.to_string());
    Ok(config)
}

fn train_case(engine: &Engine, case: &Case) -> Result<(Vocabulary, f64)> {
    let config = config_for(case, "")?;
    let documents: Vec<Vec<u8>> = case.train.iter().map(|d| d.bytes.clone()).collect();
    let start = Instant::now();
    let vocabulary = engine.train(case.algorithm, case.adapter, &case.adapter_config, &documents, &config)?;
    Ok((vocabulary, start.elapsed().as_secs_f64()))
}

fn run_case(engine: &Engine, case: &Case, iterations: usize) -> Result<(CaseResult, Vocabulary)> {
    let (vocabulary, train_seconds) = train_case(engine, case)?;
    let trained = evaluate(engine, &vocabulary, &case.eval, iterations)?;
    let mut baseline_config = config_for(case, "-baseline")?;
    baseline_config.vocab_size = baseline_config.vocab_size.max(256);
    baseline_config.pretokenizer = Pretokenizer::None;
    let documents: Vec<Vec<u8>> = case.train.iter().map(|d| d.bytes.clone()).collect();
    let baseline_vocab = engine.train(case.baseline, case.adapter, &case.adapter_config, &documents, &baseline_config)?;
    let baseline = evaluate(engine, &baseline_vocab, &case.eval, iterations)?;
    let reduction = if trained.tokens == 0 { 0.0 } else { (baseline.tokens as f64 / trained.tokens as f64 * 10_000.0).round() / 10_000.0 };
    let result = CaseResult {
        id: case.id.to_string(),
        title: case.title.to_string(),
        dataset: DatasetInfo {
            description: case.description.clone(),
            synthetic: case.synthetic,
            train_fingerprint: fingerprint(&case.train),
            train_documents: case.train.len(),
            train_bytes: total(&case.train),
            eval_fingerprint: fingerprint(&case.eval),
            eval_documents: case.eval.len(),
            eval_bytes: total(&case.eval),
        },
        config: CaseConfig {
            algorithm: case.algorithm.to_string(),
            adapter: case.adapter.to_string(),
            adapter_options: case.adapter_config.options.clone(),
            pretokenizer: case.pretokenizer.pattern().map(str::to_string),
            requested_vocab_size: case.vocab_size,
        },
        train_seconds,
        trained,
        baseline_name: case.baseline.to_string(),
        baseline,
        token_reduction_vs_baseline: reduction,
        notes: case.notes.clone(),
    };
    Ok((result, vocabulary))
}

fn percentile(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.is_empty() {
        return 0.0;
    }
    let index = ((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1;
    v[index]
}

fn targets(engine: &Engine, cases: &[CaseResult], vocabularies: &[(String, Vocabulary)]) -> Result<Vec<TargetCheck>> {
    let mut out = Vec::new();
    let all_exact = cases.iter().all(|c| c.trained.round_trip_canonical_exact && c.baseline.round_trip_canonical_exact);
    out.push(TargetCheck {
        target: "Lossless reconstruction".into(),
        prd_value: "100%".into(),
        measured: format!("{} of {} evaluations reproduced every document's canonical form exactly", cases.iter().filter(|c| c.trained.round_trip_canonical_exact).count() + cases.iter().filter(|c| c.baseline.round_trip_canonical_exact).count(), cases.len() * 2),
        met: Some(all_exact),
        method: "decode(encode(x)) compared with the adapter's canonical form of x for every evaluation document, trained vocabulary and baseline.".into(),
    });

    // 1 KB encode latency on the first text vocabulary available.
    if let Some((name, vocabulary)) = vocabularies.iter().find(|(_, v)| v.pretokenizer.is_some()).or(vocabularies.first()) {
        let tokenizer = engine.tokenizer(vocabulary.clone())?;
        let source = data::invented_words(71, 60_000);
        let samples: Vec<&[u8]> = source.chunks_exact(1024).take(400).collect();
        for s in samples.iter().take(20) {
            std::hint::black_box(tokenizer.encode(&kute::CanonicalInput::bytes(kute::Modality::Text, *s))?);
        }
        let mut times = Vec::with_capacity(samples.len());
        for s in &samples {
            let input = kute::CanonicalInput::bytes(kute::Modality::Text, *s);
            let start = Instant::now();
            std::hint::black_box(tokenizer.encode(&input)?);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let p95 = percentile(times.clone(), 0.95);
        out.push(TargetCheck {
            target: "Encoding latency (1 KB)".into(),
            prd_value: "P95 under 5 ms".into(),
            measured: format!("P95 {:.4} ms, median {:.4} ms over {} single-threaded encodes", p95, percentile(times, 0.5), samples.len()),
            met: Some(p95 < 5.0),
            method: format!("Distinct 1024-byte samples of synthetic invented-word text, encoded one at a time with the {name} vocabulary after 20 warm-up calls. In-process call, no I/O."),
        });
    }

    // Loading a 32K vocabulary.
    let mut config = TrainingConfig::new(VocabularyId::parse("kute:bench:load-32k")?, 32_768);
    config.pretokenizer = Pretokenizer::Gpt4;
    let corpus = data::invented_words(72, 400_000);
    let start = Instant::now();
    let big = engine.train("bpe", "text", &AdapterConfig::default(), std::slice::from_ref(&corpus), &config)?;
    let train_seconds = start.elapsed().as_secs_f64();
    let file = kute::vocab::to_vec(&big)?;
    let mut load_ms = Vec::new();
    let mut ready_ms = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let loaded = kute::vocab::from_slice(&file)?;
        load_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(engine.tokenizer(loaded)?);
        ready_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let (load, ready) = (percentile(load_ms, 0.5), percentile(ready_ms, 0.5));
    out.push(TargetCheck {
        target: "32K vocabulary loading".into(),
        prd_value: "under 500 ms on reference hardware".into(),
        measured: format!(
            "{} tokens, {:.1} MiB file: parse and validate {:.0} ms median; ready-to-encode tokenizer {:.0} ms median (7 runs). Training it took {:.1} s on {:.1} MB.",
            big.tokens.len(), file.len() as f64 / 1_048_576.0, load, ready, train_seconds, corpus.len() as f64 / 1e6
        ),
        met: if big.tokens.len() >= 32_000 { Some(ready < 500.0) } else { None },
        method: "Vocabulary trained on synthetic invented-word text to reach 32,768 tokens, serialized to KVF JSON in memory, then parsed with full structural and digest validation. No reference hardware is defined yet, so this is this machine only.".into(),
    });

    // Token lookup.
    let start = Instant::now();
    let n = 200_000u32;
    for i in 0..n {
        std::hint::black_box(big.token(i % big.tokens.len() as u32)?);
    }
    let per_lookup_ns = start.elapsed().as_secs_f64() * 1e9 / n as f64;
    out.push(TargetCheck {
        target: "Vocabulary lookup".into(),
        prd_value: "P95 under 1 ms in memory".into(),
        measured: format!("mean {per_lookup_ns:.1} ns per lookup by id over {n} lookups"),
        met: Some(per_lookup_ns < 1_000_000.0),
        method: "Token-by-id lookups in the 32K vocabulary. Individual lookups are too fast to time one by one, so this is a mean, not a P95.".into(),
    });

    out.push(TargetCheck {
        target: "Determinism".into(),
        prd_value: "identical output under supported configurations".into(),
        measured: {
            let again = engine.train("bpe", "text", &AdapterConfig::default(), &[corpus], &config)?;
            if again.digest()? == big.digest()? { "retraining the 32K vocabulary reproduced the same digest".into() } else { "retraining produced a different digest".to_string() }
        },
        met: Some(true),
        method: "Two training runs on the same corpus and configuration, compared by vocabulary content digest.".into(),
    });
    if let Some(last) = out.last_mut() {
        last.met = Some(last.measured.contains("same digest"));
    }
    for (target, value) in [
        ("Distributed service availability", "99.9% production target"),
        ("Peak and average memory", "reported (FR-016)"),
    ] {
        out.push(TargetCheck {
            target: target.into(),
            prd_value: value.into(),
            measured: "not measured".into(),
            met: None,
            method: "Out of scope for v0.1: there is no service yet, and memory measurement is not implemented.".into(),
        });
    }
    Ok(out)
}

#[cfg(feature = "rustbpe")]
fn compare_backends(engine: &Engine, cases: &[Case], iterations: usize) -> Result<(Vec<BackendComparison>, String)> {
    let mut out = Vec::new();
    for case in cases.iter().filter(|c| c.algorithm == "bpe" && c.pretokenizer == Pretokenizer::Gpt4) {
        let documents: Vec<Vec<u8>> = case.train.iter().map(|d| d.bytes.clone()).collect();
        let time = |algorithm: &str, suffix: &str| -> Result<(Vocabulary, f64)> {
            let config = config_for(case, suffix)?;
            let mut times = Vec::new();
            let mut vocabulary = None;
            for _ in 0..3 {
                let start = Instant::now();
                vocabulary = Some(engine.train(algorithm, case.adapter, &case.adapter_config, &documents, &config)?);
                times.push(start.elapsed().as_secs_f64());
            }
            Ok((vocabulary.expect("three runs"), percentile(times, 0.5)))
        };
        let (native, native_s) = time("bpe", "-native")?;
        let (reference, reference_s) = time("rustbpe", "-rustbpe")?;
        let native_eval = evaluate(engine, &native, &case.eval, iterations)?;
        let reference_eval = evaluate(engine, &reference, &case.eval, iterations)?;
        let (a, b) = (engine.tokenizer(native.clone())?, engine.tokenizer(reference.clone())?);
        let mut same = true;
        for d in &case.eval {
            same &= engine.encode_raw(a.as_ref(), &d.bytes)?.ids == engine.encode_raw(b.as_ref(), &d.bytes)?.ids;
        }
        out.push(BackendComparison {
            case: case.id.to_string(),
            native_train_seconds: native_s,
            rustbpe_train_seconds: reference_s,
            merges_identical: native.merges == reference.merges,
            eval_encodings_identical: same,
            native_encode_mb_per_second: native_eval.encode_mb_per_second,
            rustbpe_encode_mb_per_second: reference_eval.encode_mb_per_second,
            note: "Train time is the median of 3 runs and includes KUTE's adapter, fingerprinting, vocabulary assembly and self-test for both backends; the rustbpe figure also includes handing the corpus to an embedded Python interpreter. The rustbpe encode path includes this backend's decode-and-compare check.".into(),
        });
    }
    Ok((out, "run".into()))
}

#[cfg(not(feature = "rustbpe"))]
fn compare_backends(_: &Engine, _: &[Case], _: usize) -> Result<(Vec<BackendComparison>, String)> {
    Ok((Vec::new(), "not run: built without the rustbpe feature (it needs a Python installation)".into()))
}

/// Run every case that can run here.
pub fn run(engine: &Engine, options: &Options) -> Result<SuiteReport> {
    let mut skipped = Vec::new();
    let cases = cases(options, &mut skipped);
    let mut results = Vec::new();
    let mut vocabularies = Vec::new();
    for case in &cases {
        let (result, vocabulary) = run_case(engine, case, options.iterations)?;
        vocabularies.push((case.id.to_string(), vocabulary));
        results.push(result);
    }
    for (id, why) in [
        ("KUTE-B06 DNA", "needs a FASTA adapter (planned v0.3)"),
        ("KUTE-B07 telemetry", "needs numerical quantization (planned v0.3)"),
        ("KUTE-B08 mathematics", "needs an expression adapter and dataset"),
        ("KUTE-B09 quantum circuits", "needs a circuit adapter and dataset"),
        ("KUTE-B10 HRM states", "research; only the adapter contract exists in v0.1"),
    ] {
        skipped.push(format!("{id}: not run, {why}"));
    }
    let targets = targets(engine, &results, &vocabularies)?;
    let (rustbpe_comparison, rustbpe_comparison_status) = compare_backends(engine, &cases, options.iterations)?;
    Ok(SuiteReport {
        report: "KUTE initial benchmark program, v0.1",
        environment: Environment::detect(),
        methodology: vec![
            "Each case trains on one dataset and is evaluated on a separate one; fingerprints of both are recorded.",
            "Compression is complete serialized size (.kute containers: header plus varint ids) against raw bytes, with and without the vocabulary file, next to gzip at its default level on the same bytes.",
            "Every evaluation document is encoded, decoded and compared; a case is exact only if all of them match.",
            "Timings are medians over the stated number of iterations, single process, on the machine described under environment. They are not comparable across machines.",
            "Cases marked synthetic use seeded template generators. Their numbers show the machinery works; they do not predict results on real data.",
            "Training is deterministic, so rerunning this suite at the same commit reproduces every count and fingerprint; only timings vary.",
        ],
        cases: results,
        skipped,
        targets,
        rustbpe_comparison,
        rustbpe_comparison_status,
    })
}
