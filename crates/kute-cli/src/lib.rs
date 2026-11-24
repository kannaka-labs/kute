//! `kute`: train, encode, decode, inspect, validate, compare, export, benchmark.
//!
//! Results go to stdout as JSON (or raw bytes for `decode`), messages to
//! stderr. Exit status is 0 on success and 1 on any error.

use clap::{Parser, Subcommand, ValueEnum};
use kute::{vocab, AdapterConfig, Engine, KuteError, Pretokenizer, TokenSequence, TrainingConfig, Version, VocabularyId};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "kute", version, about = "Kannaka Universal Tokenization Engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Learn a vocabulary from files and write it as KVF.
    Train(Box<TrainArgs>),
    /// Encode a file to a .kute token sequence.
    Encode {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long)]
        input: PathBuf,
        /// Where to write the .kute container. Without it, token ids are printed as JSON.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Write the compressed container (format 2) instead of plain varint ids.
        #[arg(long, requires = "output")]
        compress: bool,
        /// What to do with a symbol the vocabulary does not contain:
        /// `strict` refuses the input, `escape` carries the symbol literally
        /// in the sequence so it still decodes exactly.
        #[arg(long, default_value = "strict")]
        unknown: String,
    },
    /// Decode a .kute token sequence back to bytes.
    Decode {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long)]
        input: PathBuf,
        /// Where to write the decoded bytes. Without it, they go to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Refuse a sequence that would decode to more than this many bytes
        /// or symbols. Use it for sequences from an untrusted source.
        #[arg(long)]
        max_units: Option<u64>,
    },
    /// Summarize a vocabulary, or show tokens from it.
    Inspect {
        vocab: PathBuf,
        /// Show the full envelope of one token id.
        #[arg(long)]
        token: Option<u32>,
        /// Show the N longest learned tokens.
        #[arg(long, default_value_t = 0)]
        longest: usize,
    },
    /// Check structure, integrity and the embedded test vectors.
    Validate { vocab: PathBuf },
    /// Compare two vocabularies by token content.
    Compare { a: PathBuf, b: PathBuf },
    /// Convert a vocabulary to another tool's format.
    Export {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long, value_enum, default_value_t = ExportFormat::Tiktoken)]
        format: ExportFormat,
        /// Output file, e.g. general.tiktoken. A `.json` sidecar with the
        /// pattern and special tokens is written next to it.
        #[arg(long)]
        output: PathBuf,
    },
    /// Measure a vocabulary on a dataset, or run the benchmark suite.
    Benchmark {
        #[arg(long, required_unless_present = "suite")]
        vocab: Option<PathBuf>,
        /// File or directory to evaluate on. Keep it separate from training data.
        #[arg(long, required_unless_present = "suite")]
        dataset: Option<PathBuf>,
        /// Run the built-in benchmark program instead.
        #[arg(long, conflicts_with_all = ["vocab", "dataset"])]
        suite: bool,
        /// Repository root for the suite's in-repo corpora.
        #[arg(long)]
        repo_root: Option<PathBuf>,
        /// A large UTF-8 text file for the suite's real-data case (for example enwik8).
        /// The first 90% trains a 32K vocabulary; the rest is the evaluation set.
        #[arg(long, requires = "suite")]
        large_text: Option<PathBuf>,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        /// Also write the JSON report to this file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Find recurring patterns in symbol streams, without training a tokenizer.
    Discover(Box<DiscoverArgs>),
    /// List registered algorithms and adapters and what each declares.
    Capabilities,
    /// Write the seeded example datasets used by the documentation.
    SampleData {
        /// Directory to create the files in.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ExportFormat {
    Tiktoken,
}

#[derive(clap::Args)]
struct DiscoverArgs {
    /// Files or directories to look for patterns in.
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,
    /// Adapter that turns each file into one record of symbols.
    #[arg(long, default_value = "jsonl-events")]
    adapter: String,
    /// Adapter option as key=value. Repeatable.
    #[arg(long = "adapter-option", value_name = "KEY=VALUE")]
    adapter_options: Vec<String>,
    /// Read plain text instead of using an adapter: every line is one
    /// record, symbols separated by whitespace.
    #[arg(long, conflicts_with_all = ["adapter", "adapter_options"])]
    lines: bool,
    /// Files with held-out data to check the patterns against.
    #[arg(long, num_args = 1.., conflicts_with = "holdout_fraction")]
    holdout: Vec<PathBuf>,
    /// Hold out this share of the input instead. With --lines the last
    /// records are held out; otherwise the end of each record.
    #[arg(long)]
    holdout_fraction: Option<f64>,
    #[arg(long, default_value_t = 5)]
    min_support: u64,
    /// Longest contiguous pattern, in symbols.
    #[arg(long, default_value_t = 8)]
    max_length: usize,
    /// Minimum cohesion in bits: how much more often than chance.
    #[arg(long, default_value_t = 1.0)]
    min_cohesion: f64,
    /// Gapped pairs look this many symbols ahead; 0 turns them off.
    #[arg(long, default_value_t = 8)]
    window: usize,
    /// Report at most this many patterns of each kind.
    #[arg(long, default_value_t = 50)]
    top: usize,
    /// Also run on a shuffled copy of the input (same symbol counts, no
    /// order) and report how much the method finds there.
    #[arg(long)]
    control: bool,
    /// Also write the JSON report to this file.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn read_records(engine: &Engine, args: &DiscoverArgs, paths: &[PathBuf]) -> kute::Result<Vec<Vec<String>>> {
    let mut adapter_config = AdapterConfig::default();
    for option in &args.adapter_options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| KuteError::InvalidConfig(format!("adapter option {option:?}: expected key=value")))?;
        adapter_config = adapter_config.with(key, value);
    }
    let mut records = Vec::new();
    for path in paths {
        for file in kute::collect_files(path)? {
            let raw = std::fs::read(&file)?;
            if args.lines {
                let text = String::from_utf8(raw).map_err(|_| KuteError::InvalidInput(format!("{}: not UTF-8", file.display())))?;
                records.extend(text.lines().map(|l| l.split_whitespace().map(str::to_string).collect::<Vec<_>>()).filter(|r| !r.is_empty()));
            } else {
                let input = engine.registry().adapter(&args.adapter)?.ingest(&raw, &adapter_config)?;
                records.extend(kute_discover::records_of(&[input])?);
            }
        }
    }
    if records.is_empty() {
        return Err(KuteError::InvalidInput("no records found in the input".into()));
    }
    Ok(records)
}

fn discover(engine: &Engine, args: DiscoverArgs) -> kute::Result<()> {
    use kute_discover as kd;
    let params = kd::Params {
        min_support: args.min_support,
        max_length: args.max_length,
        min_cohesion_bits: args.min_cohesion,
        window: args.window,
        max_patterns: args.top,
        ..kd::Params::default()
    };
    let mut records = read_records(engine, &args, &args.input)?;
    let mut held_out = if args.holdout.is_empty() { None } else { Some(read_records(engine, &args, &args.holdout)?) };
    if let Some(fraction) = args.holdout_fraction {
        if args.lines {
            if !(fraction > 0.0 && fraction < 1.0) {
                return Err(KuteError::InvalidConfig("the held-out fraction must be between 0 and 1".into()));
            }
            let cut = ((records.len() as f64) * (1.0 - fraction)).round() as usize;
            held_out = Some(records.split_off(cut.clamp(1, records.len())));
        } else {
            let (head, tail) = kd::split_holdout(&records, fraction)?;
            records = head;
            held_out = Some(tail);
        }
    }
    let start = std::time::Instant::now();
    let found = kd::discover(&records, &params)?;
    let seconds = start.elapsed().as_secs_f64();
    let evaluation = held_out.as_ref().map(|h| kd::evaluate(&found, h)).transpose()?;
    let control = if args.control {
        let found = kd::discover(&kd::shuffled(&records, 0), &params)?;
        let evaluation = held_out.as_ref().map(|h| kd::evaluate(&found, h)).transpose()?;
        Some(json!({
            "what": "the same run on a shuffled copy of the input: same symbol counts, no order",
            "patterns": found.patterns.len(),
            "strongest_evidence_bits": found.patterns.iter().map(|p| p.evidence_bits).fold(0.0, f64::max),
            "coverage": found.coverage,
            "held_out_confirmed": evaluation.as_ref().map(|e| e.confirmed),
            "held_out_coverage": evaluation.as_ref().map(|e| e.coverage),
        }))
    } else {
        None
    };
    let report = json!({
        "kute_version": kute::KUTE_VERSION,
        "discover_seconds": seconds,
        "discovery": found,
        "held_out": evaluation,
        "control": control,
    });
    if let Some(path) = &args.output {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    print(&report)
}

#[derive(clap::Args)]
struct TrainArgs {
    /// Files or directories to train on. Directories are read recursively.
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,
    /// Vocabulary identifier, e.g. kute:industrial:events.
    #[arg(long)]
    id: String,
    /// Vocabulary version (semantic versioning).
    #[arg(long, default_value = "0.1.0")]
    vocab_version: String,
    #[arg(long, default_value = "bpe")]
    algorithm: String,
    #[arg(long, default_value = "text")]
    adapter: String,
    /// Adapter option as key=value. Repeatable.
    #[arg(long = "adapter-option", value_name = "KEY=VALUE")]
    adapter_options: Vec<String>,
    #[arg(long, default_value_t = 4096)]
    vocab_size: u32,
    /// none, gpt4, or regex:<pattern>. Default: gpt4 for the text adapter, none otherwise.
    #[arg(long)]
    pretokenizer: Option<String>,
    #[arg(long, default_value_t = 1)]
    min_frequency: u64,
    #[arg(long)]
    max_token_length: Option<usize>,
    /// Reserved (special) token. Repeatable.
    #[arg(long = "reserved")]
    reserved: Vec<String>,
    /// File with one dictionary entry per line, for the dictionary algorithm.
    #[arg(long)]
    dictionary_file: Option<PathBuf>,
    #[arg(long)]
    description: Option<String>,
    /// License of the vocabulary, e.g. an SPDX identifier.
    #[arg(long)]
    license: Option<String>,
    /// Do not store excerpts of the training data in the vocabulary as
    /// self-test vectors. Use it when the corpus is private.
    #[arg(long)]
    no_test_vectors: bool,
    #[arg(long)]
    output: PathBuf,
    /// Replace an existing output file even if its content differs.
    #[arg(long)]
    force: bool,
}

fn print(value: &impl serde::Serialize) -> kute::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}")?;
    Ok(())
}

fn parse_pretokenizer(spec: Option<&str>, adapter: &str) -> kute::Result<Pretokenizer> {
    match spec {
        None => Ok(if adapter == "text" { Pretokenizer::Gpt4 } else { Pretokenizer::None }),
        Some("none") => Ok(Pretokenizer::None),
        Some("gpt4") => Ok(Pretokenizer::Gpt4),
        Some(s) => match s.strip_prefix("regex:") {
            Some(p) if !p.is_empty() => Ok(Pretokenizer::Regex(p.to_string())),
            _ => Err(KuteError::InvalidConfig(format!("pretokenizer {s:?}: expected none, gpt4 or regex:<pattern>"))),
        },
    }
}

fn train(engine: &Engine, args: TrainArgs) -> kute::Result<()> {
    let mut config = TrainingConfig::new(VocabularyId::parse(&args.id)?, args.vocab_size);
    config.vocabulary_version = Version::parse(&args.vocab_version)
        .map_err(|e| KuteError::InvalidIdentifier(format!("version {:?}: {e}", args.vocab_version)))?;
    let symbol_algorithm = args.algorithm == "sequence-bpe";
    config.pretokenizer = if symbol_algorithm && args.pretokenizer.is_none() {
        Pretokenizer::None
    } else {
        parse_pretokenizer(args.pretokenizer.as_deref(), &args.adapter)?
    };
    config.min_frequency = args.min_frequency;
    config.max_token_length = args.max_token_length;
    config.reserved_tokens = args.reserved;
    config.description = args.description;
    config.license = args.license;
    config.embed_test_vectors = !args.no_test_vectors;
    if let Some(path) = &args.dictionary_file {
        let text = std::fs::read_to_string(path)?;
        config.dictionary = text.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).map(str::to_string).collect();
    }
    let mut adapter_config = AdapterConfig::default();
    for option in &args.adapter_options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| KuteError::InvalidConfig(format!("adapter option {option:?}: expected key=value")))?;
        adapter_config = adapter_config.with(key, value);
    }
    let start = std::time::Instant::now();
    let vocabulary = engine.train_paths(&args.algorithm, &args.adapter, &adapter_config, &args.input, &config)?;
    let seconds = start.elapsed().as_secs_f64();
    vocab::save(&vocabulary, &args.output, args.force)?;
    eprintln!("trained {} {} in {seconds:.2} s -> {}", vocabulary.id(), vocabulary.version(), args.output.display());
    print(&json!({
        "output": args.output,
        "train_seconds": seconds,
        "summary": vocab::summarize(&vocabulary)?,
    }))
}

fn run(engine: Engine, cli: Cli) -> kute::Result<()> {
    match cli.command {
        Command::Train(args) => train(&engine, *args),
        Command::Encode { vocab: vocab_path, input, output, compress, unknown } => {
            let policy = kute::UnknownPolicy::parse(&unknown)?;
            let tokenizer = engine.tokenizer(vocab::load(&vocab_path)?)?;
            let raw = std::fs::read(&input)?;
            let sequence = engine.encode_raw_open(tokenizer.as_ref(), &raw, policy)?;
            match output {
                Some(path) => {
                    let bytes = if compress { sequence.to_bytes_compressed()? } else { sequence.to_bytes()? };
                    std::fs::write(&path, &bytes)?;
                    print(&json!({
                        "output": path,
                        "input_bytes": raw.len(),
                        "tokens": sequence.ids.len(),
                        "escaped_symbols": sequence.escapes.len(),
                        "serialized_bytes": bytes.len(),
                        "vocabulary": format!("{} {}", sequence.vocabulary_id, sequence.vocabulary_version),
                    }))
                }
                None => print(&sequence),
            }
        }
        Command::Decode { vocab: vocab_path, input, output, max_units } => {
            let tokenizer = engine.tokenizer(vocab::load(&vocab_path)?)?;
            let sequence = TokenSequence::from_bytes(&std::fs::read(&input)?)?;
            let bytes = engine.decode_raw_bounded(tokenizer.as_ref(), &sequence, max_units.unwrap_or(u64::MAX))?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &bytes)?;
                    print(&json!({ "output": path, "bytes": bytes.len(), "tokens": sequence.ids.len() }))
                }
                None => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(&bytes)?;
                    out.flush()?;
                    Ok(())
                }
            }
        }
        Command::Inspect { vocab: vocab_path, token, longest } => {
            let vocabulary = vocab::load(&vocab_path)?;
            if let Some(id) = token {
                return print(&vocabulary.record(id)?);
            }
            let mut report = json!({ "summary": vocab::summarize(&vocabulary)? });
            if longest > 0 {
                let mut learned: Vec<&kute::TokenEntry> = vocabulary.tokens.iter().filter(|t| t.kind == kute::TokenKind::Merge).collect();
                learned.sort_by(|a, b| b.payload.len().cmp(&a.payload.len()).then(a.id.cmp(&b.id)));
                let rows: Vec<kute::TokenRecord> = learned.into_iter().take(longest).map(|t| vocabulary.record(t.id)).collect::<kute::Result<_>>()?;
                report["longest_learned_tokens"] = serde_json::to_value(rows)?;
            }
            print(&report)
        }
        Command::Validate { vocab: vocab_path } => {
            let vocabulary = vocab::load(&vocab_path)?;
            let report = engine.verify(&vocabulary)?;
            print(&report)?;
            if report.ok() {
                Ok(())
            } else {
                Err(KuteError::InvalidVocabulary(format!("{} of {} test vectors failed", report.failures.len(), report.vectors)))
            }
        }
        Command::Compare { a, b } => print(&vocab::compare(&vocab::load(&a)?, &vocab::load(&b)?)?),
        Command::Export { vocab: vocab_path, format: ExportFormat::Tiktoken, output } => {
            let vocabulary = vocab::load(&vocab_path)?;
            let export = vocab::export_tiktoken(&vocabulary)?;
            std::fs::write(&output, &export.bpe_file)?;
            let mut sidecar = output.clone().into_os_string();
            sidecar.push(".json");
            let sidecar = PathBuf::from(sidecar);
            let meta = json!({
                "name": vocabulary.id().as_str().replace(':', "_"),
                "pat_str": export.pat_str,
                "special_tokens": export.special_tokens,
                "explicit_n_vocab": export.n_vocab,
                "source_vocabulary": format!("{} {}", vocabulary.id(), vocabulary.version()),
                "source_digest": vocabulary.digest()?,
            });
            std::fs::write(&sidecar, serde_json::to_vec_pretty(&meta)?)?;
            print(&json!({ "bpe_file": output, "metadata": sidecar, "tokens": export.n_vocab }))
        }
        Command::Benchmark { vocab: vocab_path, dataset, suite, repo_root, large_text, iterations, output } => {
            let report = if suite {
                serde_json::to_value(kute_bench::suite::run(&engine, &kute_bench::suite::Options { repo_root, iterations, large_text })?)?
            } else {
                let (Some(vocab_path), Some(dataset)) = (vocab_path, dataset) else {
                    return Err(KuteError::InvalidConfig("benchmark needs --vocab and --dataset, or --suite".into()));
                };
                let vocabulary = vocab::load(&vocab_path)?;
                let documents = kute_bench::read_dataset(&dataset)?;
                json!({
                    "environment": kute_bench::Environment::detect(),
                    "dataset": dataset,
                    "evaluation": kute_bench::evaluate(&engine, &vocabulary, &documents, iterations)?,
                })
            };
            if let Some(path) = output {
                let mut text = serde_json::to_string_pretty(&report)?;
                text.push('\n');
                std::fs::write(&path, text)?;
                eprintln!("report written to {}", path.display());
            }
            print(&report)
        }
        Command::SampleData { output } => {
            use kute_bench::data;
            std::fs::create_dir_all(&output)?;
            let files: Vec<(&str, Vec<u8>, &str)> = vec![
                ("alarms-train.jsonl", data::alarm_log(51, 4000), "plant alarm log, 4000 episodes"),
                ("alarms-eval.jsonl", data::alarm_log(52, 1000), "plant alarm log, 1000 episodes, different seed"),
                ("messages-train.jsonl", data::agent_messages(61, 4000), "agent messages, 4000 records"),
                ("messages-eval.jsonl", data::agent_messages(62, 1000), "agent messages, 1000 records, different seed"),
                ("frames-train.bin", data::binary_records(31, 6000), "binary telemetry frames, 6000 records"),
                ("frames-eval.bin", data::binary_records(32, 2000), "binary telemetry frames, 2000 records, different seed"),
                ("words-train.txt", data::invented_words(81, 60_000), "invented-word text, 60000 words"),
                ("words-eval.txt", data::invented_words(82, 15_000), "invented-word text, 15000 words, different seed"),
            ];
            let mut written = Vec::new();
            for (name, bytes, what) in files {
                std::fs::write(output.join(name), &bytes)?;
                written.push(json!({ "file": name, "bytes": bytes.len(), "sha256": kute::sha256_tagged(&bytes), "content": what }));
            }
            print(&json!({ "output": output, "synthetic": true, "files": written }))
        }
        Command::Discover(args) => discover(&engine, *args),
        Command::Capabilities => {
            let registry = engine.registry();
            let algorithms: Vec<kute::TokenizerCapabilities> =
                registry.algorithm_names().iter().map(|n| registry.tokenizer(n).map(|t| t.capabilities())).collect::<kute::Result<_>>()?;
            let adapters: Vec<serde_json::Value> = registry
                .adapter_names()
                .iter()
                .map(|n| {
                    registry.adapter(n).map(|a| {
                        let c = a.capabilities();
                        json!({ "name": c.name, "modality": c.modality, "lossless": c.lossless, "notes": c.notes })
                    })
                })
                .collect::<kute::Result<_>>()?;
            print(&json!({
                "kute_version": kute::KUTE_VERSION,
                "kvf_version": kute::KVF_VERSION,
                "algorithms": algorithms,
                "adapters": adapters,
            }))
        }
    }
}

/// Parse the command line and run it on `engine`. The `kute` binary passes
/// the default engine; other front ends can register more algorithms first.
pub fn main_with(engine: Engine) -> ExitCode {
    match run(engine, Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kute: {e}");
            ExitCode::FAILURE
        }
    }
}
