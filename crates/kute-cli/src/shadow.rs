//! `kute shadow`: try a candidate vocabulary beside the one in use, on
//! data neither was made from, and say whether it has earned its place.
//!
//! Every input is encoded with both vocabularies. Nothing is published,
//! replaced or sent: the output is a decision with the measurements it
//! rests on. The candidate is approved only if what it saves, after the
//! cost of distributing it, is at least the improvement asked for, and
//! no constraint is broken.

use crate::vocab;
use kute::{KuteError, UnknownPolicy};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

pub struct Options {
    pub parent: PathBuf,
    pub candidate: PathBuf,
    /// Held-out inputs, each encoded as one message.
    pub inputs: Vec<PathBuf>,
    pub unknown: String,
    /// Least net saving, as a share of the parent's bytes, to approve.
    pub min_improvement: f64,
    /// How much traffic the candidate will carry, as a multiple of the inputs.
    pub horizon: f64,
    /// How many times the candidate vocabulary has to be sent.
    pub distributions: u64,
    /// Most the candidate may slow encoding, as a ratio.
    pub max_slowdown: f64,
    /// Timed passes over the inputs; the median is used.
    pub passes: usize,
}

struct Side {
    tokens: u64,
    escaped: u64,
    container_bytes: u64,
    seconds: f64,
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Encode every input, check each decodes to its source, and time it.
fn measure(engine: &kute::Engine, vocabulary: kute::Vocabulary, raws: &[Vec<u8>], policy: UnknownPolicy, passes: usize) -> kute::Result<Result<Side, String>> {
    let tokenizer = engine.tokenizer(vocabulary)?;
    let mut side = Side { tokens: 0, escaped: 0, container_bytes: 0, seconds: 0.0 };
    for (i, raw) in raws.iter().enumerate() {
        let sequence = match engine.encode_raw_open(tokenizer.as_ref(), raw, policy) {
            Ok(sequence) => sequence,
            // With `strict`, a symbol the vocabulary lacks ends here: a
            // finding about the vocabulary, not a failure of the command.
            Err(e) => return Ok(Err(format!("input {}: {e}", i + 1))),
        };
        side.tokens += sequence.ids.len() as u64;
        side.escaped += sequence.escapes.len() as u64;
        side.container_bytes += sequence.to_bytes_compressed()?.len() as u64;
        if engine.decode_raw_bounded(tokenizer.as_ref(), &sequence, u64::MAX)? != *raw {
            return Ok(Err(format!("input {} does not decode to itself", i + 1)));
        }
    }
    let mut timings = Vec::with_capacity(passes);
    for _ in 0..passes {
        let started = Instant::now();
        for raw in raws {
            std::hint::black_box(engine.encode_raw_open(tokenizer.as_ref(), raw, policy)?);
        }
        timings.push(started.elapsed().as_secs_f64());
    }
    side.seconds = median(timings);
    Ok(Ok(side))
}

fn describe(side: &Side) -> Value {
    json!({ "tokens": side.tokens, "escaped_symbols": side.escaped, "container_bytes": side.container_bytes, "encode_seconds": (side.seconds * 1e6).round() / 1e6 })
}

/// Returns the report and whether the candidate was approved.
pub fn run(engine: &kute::Engine, options: &Options) -> kute::Result<(Value, bool)> {
    let config = |why: &str| Err(KuteError::InvalidConfig(why.into()));
    if options.inputs.is_empty() {
        return config("shadow needs at least one input");
    }
    if !(options.min_improvement.is_finite() && (0.0..1.0).contains(&options.min_improvement)) {
        return config("--min-improvement is a share between 0 and 1");
    }
    if !(options.horizon.is_finite() && options.horizon > 0.0 && options.max_slowdown.is_finite() && options.max_slowdown >= 1.0) {
        return config("--horizon must be positive and --max-slowdown at least 1");
    }
    if options.passes == 0 || options.passes > 99 {
        return config("--passes must be between 1 and 99");
    }
    let policy = UnknownPolicy::parse(&options.unknown)?;
    let parent = vocab::load(&options.parent)?;
    let candidate = vocab::load(&options.candidate)?;
    let parent_digest = parent.digest()?;
    let candidate_digest = candidate.digest()?;
    let declared_parent = vocab::parent_digest(&candidate);
    let candidate_file_bytes = std::fs::metadata(&options.candidate)?.len();
    let (parent_tokens, candidate_tokens) = (parent.len(), candidate.len());
    let raws = options.inputs.iter().map(std::fs::read).collect::<std::io::Result<Vec<_>>>()?;
    let input_bytes: u64 = raws.iter().map(|r| r.len() as u64).sum();

    let mut reasons: Vec<String> = Vec::new();
    if parent_digest == candidate_digest {
        reasons.push("the candidate is the parent".into());
    }
    let lineage = match &declared_parent {
        Some(digest) if *digest == parent_digest => "child of the parent",
        Some(_) => "child of another vocabulary",
        None => "no parent recorded",
    };
    let before = match measure(engine, parent, &raws, policy, options.passes)? {
        Ok(side) => side,
        Err(why) => return Err(KuteError::InvalidInput(format!("the parent vocabulary cannot carry the inputs: {why}"))),
    };
    let after = measure(engine, candidate, &raws, policy, options.passes)?;

    let mut report = json!({
        "parent": { "path": options.parent, "digest": parent_digest, "tokens_in_vocabulary": parent_tokens, "measured": describe(&before) },
        "candidate": { "path": options.candidate, "digest": candidate_digest, "tokens_in_vocabulary": candidate_tokens, "file_bytes": candidate_file_bytes, "lineage": lineage },
        "inputs": { "files": raws.len(), "bytes": input_bytes },
        "terms": {
            "min_improvement": options.min_improvement,
            "horizon": options.horizon,
            "distributions": options.distributions,
            "max_slowdown": options.max_slowdown,
            "unknown": options.unknown,
            "passes": options.passes,
        },
    });
    match after {
        Err(why) => reasons.push(format!("the candidate cannot carry the inputs: {why}")),
        Ok(after) => {
            // What the inputs stand for, scaled to the traffic expected,
            // less what it costs to hand the candidate out.
            let saved_on_inputs = before.container_bytes as f64 - after.container_bytes as f64;
            let distribution_cost = options.distributions as f64 * candidate_file_bytes as f64;
            let net = options.horizon * saved_on_inputs - distribution_cost;
            let basis = options.horizon * before.container_bytes as f64;
            let improvement = if basis > 0.0 { net / basis } else { 0.0 };
            let slowdown = if before.seconds > 0.0 { after.seconds / before.seconds } else { 1.0 };
            if improvement < options.min_improvement {
                reasons.push(format!("net improvement {:.4} is below the {:.4} required", improvement, options.min_improvement));
            }
            if slowdown > options.max_slowdown {
                reasons.push(format!("encoding is {slowdown:.2} times slower; the limit is {:.2}", options.max_slowdown));
            }
            if after.escaped > before.escaped {
                reasons.push(format!("the candidate escapes more symbols ({} against {})", after.escaped, before.escaped));
            }
            report["candidate"]["measured"] = describe(&after);
            report["comparison"] = json!({
                "container_bytes_saved_on_inputs": saved_on_inputs,
                "container_ratio": if before.container_bytes > 0 { after.container_bytes as f64 / before.container_bytes as f64 } else { 1.0 },
                "token_ratio": if before.tokens > 0 { after.tokens as f64 / before.tokens as f64 } else { 1.0 },
                "distribution_cost_bytes": distribution_cost,
                "net_bytes_saved_over_horizon": net,
                "net_improvement": improvement,
                "encode_slowdown": (slowdown * 1000.0).round() / 1000.0,
                // The inputs it would take, at this rate of saving, to pay
                // for distributing the candidate; absent when it saves nothing.
                "break_even_horizon": if saved_on_inputs > 0.0 { Some(distribution_cost / saved_on_inputs) } else { None },
            });
        }
    }
    let approved = reasons.is_empty();
    report["decision"] = json!(if approved { "approve" } else { "reject" });
    report["reasons"] = json!(reasons);
    report["not_measured"] = json!("memory, task quality and predictive utility; timing is wall-clock on this machine");
    Ok((report, approved))
}
