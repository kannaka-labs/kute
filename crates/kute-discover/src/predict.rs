//! How much the recent past of an entity says about its next event,
//! measured on time that was held out.
//!
//! [`evaluate`] cuts a timed event stream at one moment. Everything
//! before it is used to count; every event at or after it is predicted,
//! one at a time, from the events of the same entity that came before.
//! Three predictors are compared, all built the same way and differing
//! only in how much of the past they look at:
//!
//! * `frequency`: no past at all, only how common each event is;
//! * `previous`: the one event before (for an entity's first event,
//!   the fact that it is the first);
//! * `context`: up to `max_context` events before, using the longest
//!   run seen at least `min_count` times.
//!
//! The first two are the baselines. What the third adds over them is the
//! measurement. For each: the average surprise in bits an event (lower is
//! better), how often the most likely event was the one that came, and
//! whether the probabilities it states are borne out (calibration).
//!
//! What the design controls for:
//!
//! * **Overlap.** The cut is in time, so nothing counted comes after
//!   anything predicted.
//! * **Entity leakage.** An entity seen before the cut may be easy to
//!   predict after it for reasons particular to itself. The report gives
//!   the scores over all held-out events and again over entities never
//!   seen before the cut.
//! * **Drift.** The held-out time is cut into equal parts and each is
//!   scored, so a predictor that decays shows it.
//! * **Repeated testing.** The three predictors and every setting are
//!   fixed before the held-out events are read; nothing is chosen by its
//!   score. Differences come with an interval from resampling entities.
//!   That covers one call. Running it again with other settings and
//!   keeping the best is repeated testing, and nothing here can see it.
//!
//! What it does not show: that any event causes another. A sequence that
//! predicts the next event is an association in this data.

use kute_types::{EventStream, KuteError, Result};
use rustc_hash::FxHashMap;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PredictParams {
    /// Share of the events, latest in time, that are held out.
    pub holdout_fraction: f64,
    /// Most events of the past the `context` predictor looks at (1 to 8).
    pub max_context: usize,
    /// A run of past events is used only if seen this often before the cut.
    pub min_count: u64,
    /// Parts the held-out time is cut into for the drift table.
    pub windows: usize,
    /// Resamples of the held-out entities for the intervals.
    pub resamples: usize,
    pub seed: u64,
}

impl Default for PredictParams {
    fn default() -> Self {
        Self { holdout_fraction: 0.25, max_context: 3, min_count: 5, windows: 4, resamples: 1000, seed: 1 }
    }
}

/// Most distinct runs of past events counted.
const MAX_CONTEXTS: usize = 5_000_000;
const BINS: usize = 10;
/// Stands before an entity's first event, so that "nothing came before"
/// is a past like any other and the first event can be predicted from it.
const START: u32 = u32::MAX - 1;
/// An event name first met after the cut.
const UNKNOWN: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Score {
    pub events: u64,
    /// Average surprise: mean of -log2 of the probability given to the event that came.
    pub bits_per_event: f64,
    /// Share of events where the most likely event was the one that came.
    pub top1_accuracy: f64,
    /// Mean gap between stated confidence and how often it was right, over ten bins of confidence.
    pub calibration_error: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bin {
    /// Confidence from `from` up to `from + 0.1`.
    pub from: f64,
    pub events: u64,
    pub mean_confidence: f64,
    pub accuracy: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Difference {
    /// Bits an event the first saves over the second; positive means the first predicts better.
    pub bits_per_event: f64,
    /// The middle 95% of the same difference over resampled entities.
    pub interval_95: (f64, f64),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Window {
    pub from_time: i64,
    pub to_time: i64,
    pub events: u64,
    pub frequency_bits: f64,
    pub previous_bits: f64,
    pub context_bits: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PredictReport {
    pub params: PredictParams,
    /// Events before this time are counted; events at or after it are predicted.
    pub cut_time: i64,
    pub counted_events: u64,
    pub counted_entities: u64,
    pub alphabet: u64,
    pub contexts_counted: u64,
    pub held_out_events: u64,
    pub held_out_entities: u64,
    /// Held-out entities with no event before the cut.
    pub unseen_entities: u64,
    /// Held-out events whose name never occurred before the cut.
    pub unseen_symbol_events: u64,
    pub frequency: Score,
    pub previous: Score,
    pub context: Score,
    /// The same three over entities with no event before the cut.
    pub unseen_entities_only: Option<[Score; 3]>,
    pub context_over_previous: Difference,
    pub previous_over_frequency: Difference,
    /// Calibration of the `context` predictor, by confidence.
    pub context_calibration: Vec<Bin>,
    /// The held-out time in equal parts, in order.
    pub windows: Vec<Window>,
    /// How many predictors were compared; none was chosen by its score.
    pub predictors_compared: u32,
    pub note: &'static str,
}

/// What followed one run of past events: how often it was seen, and
/// each next event with its count.
type Followers = (u64, FxHashMap<u32, u64>);

/// Counts of what followed each run of past events.
struct Counts {
    /// Indexed by context length.
    by_length: Vec<FxHashMap<Box<[u32]>, Followers>>,
    alphabet: u64,
    contexts: usize,
}

impl Counts {
    fn add(&mut self, context: &[u32], next: u32) -> Result<()> {
        let table = &mut self.by_length[context.len()];
        if !table.contains_key(context) {
            self.contexts += 1;
            if self.contexts > MAX_CONTEXTS {
                return Err(KuteError::LimitExceeded("more than 5,000,000 distinct contexts; use a smaller max_context".into()));
            }
            table.insert(context.into(), (0, FxHashMap::default()));
        }
        let entry = table.get_mut(context).expect("just inserted");
        entry.0 += 1;
        *entry.1.entry(next).or_insert(0) += 1;
        Ok(())
    }

    /// Probability of each event after `past`, looking at most `order`
    /// events back: the longest counted context seen at least `min_count`
    /// times, each level smoothed toward the one below it, the lowest
    /// toward equal shares over the known events and one unknown.
    /// Returns the probability of `next` and the most likely event with
    /// its probability.
    fn predict(&self, past: &[u32], order: usize, min_count: u64, next: Option<u32>) -> (f64, u32, f64) {
        let slots = self.alphabet as f64 + 1.0;
        let mut depth = order.min(past.len());
        while depth > 0 && self.by_length[depth].get(&past[past.len() - depth..]).is_none_or(|entry| entry.0 < min_count) {
            depth -= 1;
        }
        // Probability of one event under the context of length `k`.
        let chance = |event: Option<u32>, upto: usize| -> f64 {
            let mut p = 1.0 / slots;
            for k in 0..=upto {
                if let Some((total, nexts)) = self.by_length[k].get(&past[past.len() - k..]) {
                    let seen = event.and_then(|e| nexts.get(&e)).copied().unwrap_or(0);
                    p = (seen as f64 + p) / (*total as f64 + 1.0);
                }
            }
            p
        };
        let p_next = chance(next, depth);
        // The most likely event is among those seen after the deepest
        // context, or after a shallower one.
        let mut best = (u32::MAX, 0.0f64);
        for k in (0..=depth).rev() {
            if let Some((_, nexts)) = self.by_length[k].get(&past[past.len() - k..]) {
                for &event in nexts.keys() {
                    let p = chance(Some(event), depth);
                    if p > best.1 || (p == best.1 && event < best.0) {
                        best = (event, p);
                    }
                }
            }
        }
        (p_next, best.0, best.1)
    }
}

/// Running sums for one predictor.
#[derive(Clone, Default)]
struct Tally {
    events: u64,
    bits: f64,
    correct: u64,
    bins: [(u64, f64, u64); BINS],
}

impl Tally {
    fn add(&mut self, p_next: f64, confidence: f64, right: bool) {
        self.events += 1;
        self.bits -= p_next.log2();
        self.correct += right as u64;
        let bin = ((confidence * BINS as f64) as usize).min(BINS - 1);
        self.bins[bin].0 += 1;
        self.bins[bin].1 += confidence;
        self.bins[bin].2 += right as u64;
    }

    fn score(&self) -> Score {
        if self.events == 0 {
            return Score::default();
        }
        let n = self.events as f64;
        let gap: f64 = self.bins.iter().filter(|b| b.0 > 0).map(|b| (b.1 - b.2 as f64).abs()).sum();
        Score { events: self.events, bits_per_event: self.bits / n, top1_accuracy: self.correct as f64 / n, calibration_error: gap / n }
    }

    fn calibration(&self) -> Vec<Bin> {
        self.bins.iter().enumerate().filter(|(_, b)| b.0 > 0).map(|(i, b)| Bin { from: i as f64 / BINS as f64, events: b.0, mean_confidence: b.1 / b.0 as f64, accuracy: b.2 as f64 / b.0 as f64 }).collect()
    }
}

struct Xorshift(u64);

impl Xorshift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// The middle 95% of `sum(a - b) / sum(events)` over resampled entities.
fn interval(per_entity: &[(f64, f64, u64)], resamples: usize, seed: u64, which: fn(&(f64, f64, u64)) -> f64) -> (f64, f64) {
    if per_entity.is_empty() || resamples == 0 {
        return (0.0, 0.0);
    }
    let mut rng = Xorshift(seed | 1);
    let mut values = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let (mut bits, mut events) = (0.0, 0u64);
        for _ in 0..per_entity.len() {
            let entity = &per_entity[rng.below(per_entity.len())];
            bits += which(entity);
            events += entity.2;
        }
        values.push(bits / events.max(1) as f64);
    }
    values.sort_by(f64::total_cmp);
    (values[(values.len() as f64 * 0.025) as usize], values[((values.len() as f64 * 0.975) as usize).min(values.len() - 1)])
}

pub fn evaluate(stream: &EventStream, params: &PredictParams) -> Result<PredictReport> {
    let bad = |why: &str| Err(KuteError::InvalidConfig(why.into()));
    if !stream.timed() {
        return bad("prediction on held-out time needs events with times");
    }
    if !(params.holdout_fraction > 0.0 && params.holdout_fraction < 1.0) {
        return bad("holdout_fraction must be between 0 and 1");
    }
    if !(1..=8).contains(&params.max_context) || params.min_count == 0 || !(1..=1000).contains(&params.windows) || params.resamples > 100_000 {
        return bad("max_context is 1 to 8, min_count at least 1, windows 1 to 1000, resamples at most 100,000");
    }
    let mut times: Vec<i64> = stream.records.iter().flat_map(|r| r.times.iter().copied()).collect();
    if times.len() < 2 {
        return Err(KuteError::InvalidInput("fewer than two events".into()));
    }
    times.sort_unstable();
    let cut_time = times[((times.len() as f64 * (1.0 - params.holdout_fraction)) as usize).min(times.len() - 1)];
    let held: Vec<i64> = times.iter().copied().filter(|&t| t >= cut_time).collect();
    if held.len() == times.len() {
        return Err(KuteError::InvalidInput("every event is at or after the cut: the times do not spread enough to hold any out".into()));
    }
    drop(times);
    // Window edges by count, so each part of the held-out time has about
    // as many events.
    let edges: Vec<i64> = (1..params.windows).map(|w| held[w * held.len() / params.windows]).collect();
    let window_of = |t: i64| edges.partition_point(|&edge| edge <= t);

    let mut ids: FxHashMap<&str, u32> = FxHashMap::default();
    let mut counts = Counts { by_length: (0..=params.max_context).map(|_| FxHashMap::default()).collect(), alphabet: 0, contexts: 0 };
    let (mut counted_events, mut counted_entities) = (0u64, 0u64);
    // Records are in time order within themselves, so each splits at one index.
    let splits: Vec<usize> = stream.records.iter().map(|r| r.times.partition_point(|&t| t < cut_time)).collect();
    // Every name that occurred before the cut, in any record, is known
    // before any held-out event is given its id. Done in one pass with
    // the counting, a record listed early would have its held-out events
    // marked unknown for names that a later record used before the cut.
    for (record, &split) in stream.records.iter().zip(&splits) {
        for symbol in &record.symbols[..split] {
            if !ids.contains_key(symbol.as_str()) {
                ids.insert(symbol.as_str(), ids.len() as u32);
            }
        }
    }
    let mut encoded: Vec<Vec<u32>> = Vec::with_capacity(stream.records.len());
    for (record, &split) in stream.records.iter().zip(&splits) {
        // Position `i + 1` holds event `i`; position 0 holds the start.
        let mut sequence = Vec::with_capacity(record.symbols.len() + 1);
        sequence.push(START);
        for symbol in &record.symbols {
            // A name never met before the cut is unknown to every predictor.
            sequence.push(ids.get(symbol.as_str()).copied().unwrap_or(UNKNOWN));
        }
        if split > 0 {
            counted_entities += 1;
        }
        for at in 1..=split {
            counted_events += 1;
            for k in 0..=params.max_context.min(at) {
                counts.add(&sequence[at - k..at], sequence[at])?;
            }
        }
        encoded.push(sequence);
    }
    counts.alphabet = ids.len() as u64;

    let orders = [0, 1, params.max_context];
    let mut all = [Tally::default(), Tally::default(), Tally::default()];
    let mut unseen = [Tally::default(), Tally::default(), Tally::default()];
    let mut windows: Vec<(u64, [f64; 3], i64, i64)> = vec![(0, [0.0; 3], i64::MAX, i64::MIN); params.windows];
    // Per held-out entity: bits saved by context over previous, by
    // previous over frequency, and its events.
    let mut per_entity: Vec<(f64, f64, u64)> = Vec::new();
    let (mut held_out_entities, mut unseen_entities, mut unseen_symbol_events) = (0u64, 0u64, 0u64);
    for ((record, sequence), &split) in stream.records.iter().zip(&encoded).zip(&splits) {
        if split == record.symbols.len() {
            continue;
        }
        held_out_entities += 1;
        let new_entity = split == 0;
        unseen_entities += new_entity as u64;
        let mut entity = (0.0, 0.0, 0u64);
        for i in split..record.symbols.len() {
            let at = i + 1;
            let next = sequence[at];
            // The past is what the entity did before, on either side of
            // the cut, back to its start or to an event no predictor knows.
            let from = sequence[..at].iter().rposition(|&id| id == UNKNOWN).map_or(0, |unknown| unknown + 1);
            let past = &sequence[from..at];
            if next == UNKNOWN {
                unseen_symbol_events += 1;
            }
            let mut bits = [0.0; 3];
            for (m, &order) in orders.iter().enumerate() {
                let (p_next, best, confidence) = counts.predict(past, order, params.min_count, (next != UNKNOWN).then_some(next));
                let right = next != UNKNOWN && best == next;
                all[m].add(p_next, confidence, right);
                if new_entity {
                    unseen[m].add(p_next, confidence, right);
                }
                bits[m] = -p_next.log2();
            }
            let w = &mut windows[window_of(record.times[i])];
            w.0 += 1;
            for (sum, b) in w.1.iter_mut().zip(bits) {
                *sum += b;
            }
            w.2 = w.2.min(record.times[i]);
            w.3 = w.3.max(record.times[i]);
            entity.0 += bits[1] - bits[2];
            entity.1 += bits[0] - bits[1];
            entity.2 += 1;
        }
        per_entity.push(entity);
    }
    let scores = [all[0].score(), all[1].score(), all[2].score()];
    Ok(PredictReport {
        params: params.clone(),
        cut_time,
        counted_events,
        counted_entities,
        alphabet: counts.alphabet,
        contexts_counted: counts.contexts as u64,
        held_out_events: scores[0].events,
        held_out_entities,
        unseen_entities,
        unseen_symbol_events,
        context_over_previous: Difference { bits_per_event: scores[1].bits_per_event - scores[2].bits_per_event, interval_95: interval(&per_entity, params.resamples, params.seed, |e| e.0) },
        previous_over_frequency: Difference { bits_per_event: scores[0].bits_per_event - scores[1].bits_per_event, interval_95: interval(&per_entity, params.resamples, params.seed ^ 0x9e37_79b9, |e| e.1) },
        unseen_entities_only: (unseen[0].events > 0).then(|| [unseen[0].score(), unseen[1].score(), unseen[2].score()]),
        context_calibration: all[2].calibration(),
        windows: windows
            .into_iter()
            .filter(|w| w.0 > 0)
            .map(|w| Window { from_time: w.2, to_time: w.3, events: w.0, frequency_bits: w.1[0] / w.0 as f64, previous_bits: w.1[1] / w.0 as f64, context_bits: w.1[2] / w.0 as f64 })
            .collect(),
        frequency: scores[0].clone(),
        previous: scores[1].clone(),
        context: scores[2].clone(),
        predictors_compared: 3,
        note: "an event that is well predicted by what came before is associated with it in this data; nothing here shows that one causes the other",
    })
}
