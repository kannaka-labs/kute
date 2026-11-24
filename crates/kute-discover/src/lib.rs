//! Structural discovery over symbol streams (development directive v0.2,
//! R6): observe, discover, evaluate. Independent of any tokenizer.
//!
//! * [`observe`] describes a corpus: size, alphabet, entropy.
//! * [`discover`] finds contiguous sequences and gapped pairs that occur
//!   more often than their parts predict, each with support and evidence.
//! * [`evaluate`] counts the same patterns in held-out data.
//! * [`shuffled`] builds a control corpus with the same symbol frequencies
//!   and no order; discovery on it shows what chance alone produces.
//!
//! A pattern is a regularity in the order of symbols. Nothing here says what
//! a pattern means or what causes it.

use kute_core::CorpusFingerprint;
use kute_types::{CanonicalInput, KuteError, Modality, Payload, Result, TokenPayload};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

/// Longest contiguous pattern that can be asked for.
pub const MAX_PATTERN_LENGTH: usize = 64;
/// Widest window for gapped pairs.
pub const MAX_WINDOW: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// A pattern must occur at least this often.
    pub min_support: u64,
    /// Longest contiguous pattern, in symbols.
    pub max_length: usize,
    /// Minimum cohesion in bits (see [`Pattern::cohesion_bits`]).
    pub min_cohesion_bits: f64,
    /// Gapped pairs look this many symbols ahead. Zero turns them off.
    pub window: usize,
    /// Keep at most this many patterns of each kind, strongest first.
    pub max_patterns: usize,
    /// Occurrences recorded as evidence for each pattern.
    pub examples: usize,
    /// Stop with an error instead of holding more than this many distinct
    /// candidate sequences at one length. Bounds memory on hostile input.
    pub max_candidates: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self { min_support: 5, max_length: 8, min_cohesion_bits: 1.0, window: 8, max_patterns: 200, examples: 3, max_candidates: 4_000_000 }
    }
}

impl Params {
    pub fn validate(&self) -> Result<()> {
        if self.min_support < 2 {
            return Err(KuteError::InvalidConfig("min_support must be at least 2".into()));
        }
        if self.max_length < 2 || self.max_length > MAX_PATTERN_LENGTH {
            return Err(KuteError::InvalidConfig(format!("max_length must be between 2 and {MAX_PATTERN_LENGTH}")));
        }
        if self.window == 1 || self.window > MAX_WINDOW {
            return Err(KuteError::InvalidConfig(format!("window must be 0 (off) or between 2 and {MAX_WINDOW}")));
        }
        if !self.min_cohesion_bits.is_finite() {
            return Err(KuteError::InvalidConfig("min_cohesion_bits must be a finite number".into()));
        }
        if self.max_candidates == 0 {
            return Err(KuteError::InvalidConfig("max_candidates must be at least 1".into()));
        }
        Ok(())
    }
}

/// Where a pattern was seen: record number and symbol offset, both from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub record: u64,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// The symbols occur one after another.
    Contiguous,
    /// Two symbols: the second occurs after the first, not adjacent, within
    /// the window.
    Gapped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub kind: PatternKind,
    pub symbols: Vec<String>,
    /// Occurrences in the corpus. Contiguous occurrences may overlap. For a
    /// gapped pair: positions of the first symbol with the second in window.
    pub support: u64,
    /// Records that contain the pattern at least once.
    pub records: u64,
    /// How much more often the pattern occurs than its parts predict, in
    /// bits. Contiguous: the smallest, over every way to cut the pattern in
    /// two, of log2(observed / expected if the halves were independent).
    /// Gapped: log2(observed / expected from the second symbol's frequency).
    pub cohesion_bits: f64,
    /// `support * cohesion_bits`: the ranking key.
    pub evidence_bits: f64,
    /// The first places the pattern was seen.
    pub examples: Vec<Occurrence>,
    /// Content hash of the symbol sequence; the same value a vocabulary
    /// token with this payload has.
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymbolCount {
    pub symbol: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub records: u64,
    pub symbols: u64,
    pub alphabet: u64,
    /// Entropy of the symbol frequencies, bits per symbol.
    pub entropy_bits: f64,
    /// Entropy of a symbol given the one before it, bits per symbol. The gap
    /// to `entropy_bits` is how much order the stream has at distance one.
    pub conditional_entropy_bits: f64,
    /// Order-sensitive fingerprint of the corpus (same scheme as training).
    pub fingerprint: String,
    pub most_frequent: Vec<SymbolCount>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Discovery {
    pub observation: Observation,
    pub params: Params,
    /// Contiguous patterns, then gapped pairs; each strongest first.
    pub patterns: Vec<Pattern>,
    /// Share of the corpus's symbols that lie inside a contiguous pattern,
    /// matching greedily, longest first.
    pub coverage: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldOutPattern {
    /// Index into [`Discovery::patterns`].
    pub pattern: usize,
    pub support: u64,
    /// Cohesion measured on the held-out data alone, when the pattern
    /// occurs there.
    pub cohesion_bits: Option<f64>,
    /// Occurs at least twice in the held-out data with positive cohesion.
    pub confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub held_out: Observation,
    pub patterns: Vec<HeldOutPattern>,
    pub confirmed: u64,
    pub confirmed_fraction: f64,
    /// Share of held-out symbols inside a discovered contiguous pattern.
    pub coverage: f64,
}

/// Symbol records taken from canonical inputs. Byte payloads are refused.
pub fn records_of(inputs: &[CanonicalInput]) -> Result<Vec<Vec<String>>> {
    inputs
        .iter()
        .map(|input| match &input.payload {
            Payload::Symbols(s) => Ok(s.clone()),
            Payload::Bytes(_) => Err(KuteError::InvalidInput("discovery takes symbol payloads; use an adapter that produces symbols".into())),
        })
        .collect()
}

struct Indexed {
    names: Vec<String>,
    index: FxHashMap<String, u32>,
    records: Vec<Vec<u32>>,
    counts: Vec<u64>,
    total: u64,
}

/// Id given to a symbol that the index does not contain.
const UNSEEN: u32 = u32::MAX;

fn index_corpus(records: &[Vec<String>], known: Option<&FxHashMap<String, u32>>) -> Indexed {
    let mut names: Vec<String> = Vec::new();
    let mut index: FxHashMap<String, u32> = known.cloned().unwrap_or_default();
    if known.is_none() {
        let mut sorted: Vec<&str> = records.iter().flatten().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        for (i, s) in sorted.iter().enumerate() {
            index.insert(s.to_string(), i as u32);
            names.push(s.to_string());
        }
    }
    let size = index.len();
    let mut counts = vec![0u64; size];
    let mut total = 0u64;
    let mapped = records
        .iter()
        .map(|r| {
            r.iter()
                .map(|s| {
                    total += 1;
                    match index.get(s) {
                        Some(&id) => {
                            counts[id as usize] += 1;
                            id
                        }
                        None => UNSEEN,
                    }
                })
                .collect()
        })
        .collect();
    Indexed { names, index, records: mapped, counts, total }
}

fn entropy(counts: impl Iterator<Item = u64>, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    counts.filter(|&c| c > 0).map(|c| {
        let p = c as f64 / total as f64;
        -p * p.log2()
    }).sum()
}

/// Describe a corpus without looking for patterns.
pub fn observe(records: &[Vec<String>]) -> Observation {
    let mut fingerprint = CorpusFingerprint::new();
    for r in records {
        fingerprint.update(&CanonicalInput { modality: Modality::Events, payload: Payload::Symbols(r.clone()) });
    }
    let corpus = index_corpus(records, None);
    let mut pairs: FxHashMap<(u32, u32), u64> = FxHashMap::default();
    let mut firsts = vec![0u64; corpus.names.len()];
    let mut pair_total = 0u64;
    for r in &corpus.records {
        for w in r.windows(2) {
            *pairs.entry((w[0], w[1])).or_insert(0) += 1;
            firsts[w[0] as usize] += 1;
            pair_total += 1;
        }
    }
    // H(next | previous) = H(pair) - H(previous), over adjacent pairs.
    let conditional = (entropy(pairs.values().copied(), pair_total) - entropy(firsts.iter().copied(), pair_total)).max(0.0);
    let mut most: Vec<SymbolCount> =
        corpus.names.iter().zip(&corpus.counts).map(|(s, &c)| SymbolCount { symbol: s.clone(), count: c }).collect();
    most.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.symbol.cmp(&b.symbol)));
    most.truncate(10);
    Observation {
        records: records.len() as u64,
        symbols: corpus.total,
        alphabet: corpus.names.len() as u64,
        entropy_bits: entropy(corpus.counts.iter().copied(), corpus.total),
        conditional_entropy_bits: conditional,
        fingerprint: fingerprint.finish(),
        most_frequent: most,
    }
}

#[derive(Clone, Default)]
struct Stat {
    count: u64,
    records: u64,
    last_record: u64,
    examples: Vec<Occurrence>,
}

impl Stat {
    fn hit(&mut self, record: u64, offset: u64, keep: usize) {
        if self.count == 0 || self.last_record != record {
            self.records += 1;
            self.last_record = record;
        }
        self.count += 1;
        if self.examples.len() < keep {
            self.examples.push(Occurrence { record, offset });
        }
    }
}

type Level = FxHashMap<Vec<u32>, Stat>;

/// Frequent contiguous sequences, level by level: a sequence of length n is
/// counted only if both its length n-1 prefix and suffix are frequent.
/// `levels[n]` holds the frequent sequences of length n (index 0 unused).
fn frequent_levels(corpus: &Indexed, params: &Params) -> Result<Vec<Level>> {
    let mut levels: Vec<Level> = vec![Level::default(), Level::default()];
    for (id, &count) in corpus.counts.iter().enumerate() {
        if count >= params.min_support {
            levels[1].insert(vec![id as u32], Stat { count, ..Stat::default() });
        }
    }
    for n in 2..=params.max_length {
        let previous = &levels[n - 1];
        if previous.is_empty() {
            break;
        }
        let mut level = Level::default();
        for (r, record) in corpus.records.iter().enumerate() {
            for (offset, w) in record.windows(n).enumerate() {
                if !previous.contains_key(&w[..n - 1]) || !previous.contains_key(&w[1..]) {
                    continue;
                }
                match level.get_mut(w) {
                    Some(stat) => stat.hit(r as u64, offset as u64, params.examples),
                    None => {
                        if level.len() >= params.max_candidates {
                            return Err(KuteError::LimitExceeded(format!(
                                "more than {} candidate sequences of length {n}; raise min_support or lower max_length",
                                params.max_candidates
                            )));
                        }
                        let mut stat = Stat::default();
                        stat.hit(r as u64, offset as u64, params.examples);
                        level.insert(w.to_vec(), stat);
                    }
                }
            }
        }
        level.retain(|_, s| s.count >= params.min_support);
        levels.push(level);
    }
    Ok(levels)
}

/// Smallest split lift of `seq`, in bits, given counts of every contiguous
/// part. `count_of` returns 0 for a part that was not counted.
fn cohesion(seq: &[u32], count: u64, total: u64, count_of: &dyn Fn(&[u32]) -> u64) -> Option<f64> {
    let mut least = f64::INFINITY;
    for k in 1..seq.len() {
        let (left, right) = (count_of(&seq[..k]), count_of(&seq[k..]));
        if left == 0 || right == 0 {
            return None;
        }
        least = least.min((count as f64 * total as f64 / (left as f64 * right as f64)).log2());
    }
    Some(least)
}

fn hash_of(symbols: &[String]) -> String {
    TokenPayload::Symbols(symbols.to_vec()).content_hash()
}

fn rank(patterns: &mut Vec<Pattern>, keep: usize) {
    patterns.sort_by(|a, b| {
        b.evidence_bits.total_cmp(&a.evidence_bits).then_with(|| b.symbols.len().cmp(&a.symbols.len())).then_with(|| a.symbols.cmp(&b.symbols))
    });
    patterns.truncate(keep);
}

/// Gapped pair counts: for each position of A, each distinct B that occurs
/// two to `window` symbols later.
fn gapped_counts(corpus: &Indexed, window: usize, examples: usize) -> FxHashMap<(u32, u32), Stat> {
    let mut pairs: FxHashMap<(u32, u32), Stat> = FxHashMap::default();
    let mut seen: Vec<u32> = Vec::with_capacity(window);
    for (r, record) in corpus.records.iter().enumerate() {
        for i in 0..record.len() {
            let a = record[i];
            if a == UNSEEN {
                continue;
            }
            seen.clear();
            for &b in record.iter().skip(i + 2).take(window - 1) {
                if b != UNSEEN && !seen.contains(&b) {
                    seen.push(b);
                    pairs.entry((a, b)).or_default().hit(r as u64, i as u64, examples);
                }
            }
        }
    }
    pairs
}

/// Expected gapped count if B's positions were independent of A's.
fn gapped_expected(count_a: u64, count_b: u64, total: u64, window: usize) -> f64 {
    let p = count_b as f64 / total as f64;
    count_a as f64 * (1.0 - (1.0 - p).powi(window as i32 - 1))
}

/// Share of symbols inside a match, matching longest first at each position.
fn coverage(records: &[Vec<u32>], by_length: &[FxHashSet<&[u32]>], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let mut covered = 0u64;
    for record in records {
        let mut i = 0;
        'next: while i < record.len() {
            for n in (2..by_length.len()).rev() {
                if i + n <= record.len() && by_length[n].contains(&record[i..i + n]) {
                    covered += n as u64;
                    i += n;
                    continue 'next;
                }
            }
            i += 1;
        }
    }
    covered as f64 / total as f64
}

/// Find patterns in `records`.
pub fn discover(records: &[Vec<String>], params: &Params) -> Result<Discovery> {
    params.validate()?;
    if records.iter().flatten().any(|s| s.is_empty()) {
        return Err(KuteError::InvalidInput("symbols must not be empty".into()));
    }
    let observation = observe(records);
    let corpus = index_corpus(records, None);
    let levels = frequent_levels(&corpus, params)?;
    let count_of = |seq: &[u32]| levels.get(seq.len()).and_then(|l| l.get(seq)).map_or(0, |s| s.count);

    let mut patterns = Vec::new();
    for n in 2..levels.len() {
        // A sequence is dropped when a longer frequent one that extends it
        // occurs exactly as often: the longer one says everything it does.
        let mut absorbed: FxHashSet<&[u32]> = FxHashSet::default();
        if let Some(longer) = levels.get(n + 1) {
            for (seq, stat) in longer {
                for part in [&seq[..n], &seq[1..]] {
                    if count_of(part) == stat.count {
                        absorbed.insert(part);
                    }
                }
            }
        }
        for (seq, stat) in &levels[n] {
            if absorbed.contains(seq.as_slice()) {
                continue;
            }
            let Some(bits) = cohesion(seq, stat.count, corpus.total, &count_of) else { continue };
            if bits < params.min_cohesion_bits {
                continue;
            }
            let symbols: Vec<String> = seq.iter().map(|&id| corpus.names[id as usize].clone()).collect();
            patterns.push(Pattern {
                kind: PatternKind::Contiguous,
                content_hash: hash_of(&symbols),
                symbols,
                support: stat.count,
                records: stat.records,
                cohesion_bits: bits,
                evidence_bits: stat.count as f64 * bits,
                examples: stat.examples.clone(),
            });
        }
    }
    rank(&mut patterns, params.max_patterns);

    let contiguous: Vec<Vec<u32>> = patterns.iter().map(|p| p.symbols.iter().map(|s| corpus.index[s]).collect()).collect();
    // A pair that a contiguous pattern already accounts for (A and B both
    // inside it, in that order, for at least half of the pair's support) is
    // not a separate finding.
    let mut inside: FxHashMap<(u32, u32), u64> = FxHashMap::default();
    for (seq, p) in contiguous.iter().zip(&patterns) {
        for i in 0..seq.len() {
            for j in i + 2..seq.len() {
                let best = inside.entry((seq[i], seq[j])).or_insert(0);
                *best = (*best).max(p.support);
            }
        }
    }
    let mut gapped = Vec::new();
    if params.window >= 2 {
        for ((a, b), stat) in gapped_counts(&corpus, params.window, params.examples) {
            if stat.count < params.min_support || inside.get(&(a, b)).is_some_and(|&c| c * 2 >= stat.count) {
                continue;
            }
            let expected = gapped_expected(corpus.counts[a as usize], corpus.counts[b as usize], corpus.total, params.window);
            let bits = (stat.count as f64 / expected).log2();
            if bits < params.min_cohesion_bits {
                continue;
            }
            let symbols = vec![corpus.names[a as usize].clone(), corpus.names[b as usize].clone()];
            gapped.push(Pattern {
                kind: PatternKind::Gapped,
                content_hash: hash_of(&symbols),
                symbols,
                support: stat.count,
                records: stat.records,
                cohesion_bits: bits,
                evidence_bits: stat.count as f64 * bits,
                examples: stat.examples,
            });
        }
        rank(&mut gapped, params.max_patterns);
    }

    let covered = coverage(&corpus.records, &by_length(&contiguous), corpus.total);
    patterns.extend(gapped);
    Ok(Discovery { observation, params: params.clone(), patterns, coverage: covered })
}

fn by_length(sequences: &[Vec<u32>]) -> Vec<FxHashSet<&[u32]>> {
    let longest = sequences.iter().map(Vec::len).max().unwrap_or(0);
    let mut sets: Vec<FxHashSet<&[u32]>> = vec![FxHashSet::default(); longest + 1];
    for s in sequences {
        sets[s.len()].insert(s.as_slice());
    }
    sets
}

/// Count the patterns of `discovery` in `held_out`, data that was not used
/// to find them. Cohesion is measured again from held-out counts alone.
pub fn evaluate(discovery: &Discovery, held_out: &[Vec<String>]) -> Result<Evaluation> {
    let mut names: Vec<&str> = discovery.patterns.iter().flat_map(|p| p.symbols.iter().map(String::as_str)).collect();
    names.sort_unstable();
    names.dedup();
    let index: FxHashMap<String, u32> = names.iter().enumerate().map(|(i, s)| (s.to_string(), i as u32)).collect();
    let corpus = index_corpus(held_out, Some(&index));
    let to_ids = |p: &Pattern| -> Vec<u32> { p.symbols.iter().map(|s| index[s]).collect() };

    // Every contiguous part of every contiguous pattern, counted in one pass
    // per length.
    let contiguous: Vec<Vec<u32>> =
        discovery.patterns.iter().filter(|p| p.kind == PatternKind::Contiguous).map(to_ids).collect();
    let mut wanted: Vec<FxHashMap<Vec<u32>, u64>> = vec![FxHashMap::default(); contiguous.iter().map(Vec::len).max().unwrap_or(0) + 1];
    for seq in &contiguous {
        for k in 1..seq.len() {
            wanted[k].entry(seq[..k].to_vec()).or_insert(0);
            wanted[seq.len() - k].entry(seq[k..].to_vec()).or_insert(0);
        }
        wanted[seq.len()].entry(seq.clone()).or_insert(0);
    }
    for (n, level) in wanted.iter_mut().enumerate().skip(1) {
        for record in &corpus.records {
            for w in record.windows(n) {
                if let Some(c) = level.get_mut(w) {
                    *c += 1;
                }
            }
        }
    }
    let count_of = |seq: &[u32]| wanted.get(seq.len()).and_then(|l| l.get(seq)).copied().unwrap_or(0);
    let window = discovery.params.window;
    let gapped = if window >= 2 { gapped_counts(&corpus, window, 0) } else { FxHashMap::default() };

    let mut patterns = Vec::with_capacity(discovery.patterns.len());
    let mut confirmed = 0u64;
    for (i, p) in discovery.patterns.iter().enumerate() {
        let ids = to_ids(p);
        let (support, bits) = match p.kind {
            PatternKind::Contiguous => {
                let support = count_of(&ids);
                (support, if support > 0 { cohesion(&ids, support, corpus.total, &count_of) } else { None })
            }
            PatternKind::Gapped => {
                let support = gapped.get(&(ids[0], ids[1])).map_or(0, |s| s.count);
                let expected = gapped_expected(corpus.counts[ids[0] as usize], corpus.counts[ids[1] as usize], corpus.total, window);
                (support, if support > 0 && expected > 0.0 { Some((support as f64 / expected).log2()) } else { None })
            }
        };
        let ok = support >= 2 && bits.is_some_and(|b| b > 0.0);
        confirmed += ok as u64;
        patterns.push(HeldOutPattern { pattern: i, support, cohesion_bits: bits, confirmed: ok });
    }
    Ok(Evaluation {
        held_out: observe(held_out),
        confirmed,
        confirmed_fraction: if patterns.is_empty() { 0.0 } else { confirmed as f64 / patterns.len() as f64 },
        coverage: coverage(&corpus.records, &by_length(&contiguous), corpus.total),
        patterns,
    })
}

impl Discovery {
    /// The contiguous patterns as symbol sequences, strongest first: the
    /// candidates a vocabulary could adopt as tokens.
    pub fn candidates(&self) -> Vec<&[String]> {
        self.patterns.iter().filter(|p| p.kind == PatternKind::Contiguous).map(|p| p.symbols.as_slice()).collect()
    }
}

/// A control corpus: each record's symbols in a seeded random order. Symbol
/// frequencies and record lengths are kept; order is destroyed. Patterns
/// found here are what the method reports from chance alone.
pub fn shuffled(records: &[Vec<String>], seed: u64) -> Vec<Vec<String>> {
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        // splitmix64
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    records
        .iter()
        .map(|r| {
            let mut out = r.clone();
            for i in (1..out.len()).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                out.swap(i, j);
            }
            out
        })
        .collect()
}

/// Split every record at `fraction` of its length: the first part to find
/// patterns in, the rest held out. Order is kept, so the held-out part is
/// later in time than the part it is checked against.
/// Symbol records: one sequence of symbol names per record.
pub type Records = Vec<Vec<String>>;

/// Split every record at `fraction` of its length; see [`Records`].
pub fn split_holdout(records: &[Vec<String>], fraction: f64) -> Result<(Records, Records)> {
    if !(fraction > 0.0 && fraction < 1.0) {
        return Err(KuteError::InvalidConfig("the held-out fraction must be between 0 and 1".into()));
    }
    let mut head = Vec::with_capacity(records.len());
    let mut tail = Vec::with_capacity(records.len());
    for r in records {
        let cut = ((r.len() as f64) * (1.0 - fraction)).round() as usize;
        head.push(r[..cut.min(r.len())].to_vec());
        tail.push(r[cut.min(r.len())..].to_vec());
    }
    Ok((head, tail))
}
