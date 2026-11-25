//! Discovery over a stream that does not end, in memory fixed in advance
//! (v0.7 specification, C2a: a prototype).
//!
//! [`Live`] takes one event at a time, an entity and a symbol, and counts
//! the contiguous sequences each entity produces. Everything it holds is
//! bounded by [`LiveParams`]: the symbols it knows, the sequences it counts
//! at each length, and the entities whose recent symbols it remembers. When
//! a bound is reached it does not grow; it approximates, and says by how
//! much:
//!
//! * **Counts** use the Misra-Gries summary. A reported support is never
//!   above the true count. Of the sequences that reached the table it is
//!   short by at most `max_undercount` for that length, which the report
//!   carries; a sequence that reached the table more often than that is
//!   certain to be in it.
//! * **Symbols** beyond `max_symbols` are folded into one "other" symbol;
//!   sequences through it are not counted. The report says how many.
//! * **Entities** beyond `max_entities` lose their recent symbols, least
//!   recently seen first, so a sequence that spans an eviction is missed.
//!   The report says how many were evicted.
//!
//! The last two lose sequences before they are counted, and no bound on
//! that loss is computed: `support_at_most` covers the first kind of error
//! only. In the test stream that presses every limit, the planted pattern
//! occurred 99,623 times and was counted 99,102 times with
//! `support_at_most` 99,108; the other 515 were cut by evictions.
//!
//! While no bound has been reached the result is exact and equals what
//! [`crate::discover`] finds for contiguous patterns in the same data.
//!
//! Not here yet: gapped pairs, event time and late events (events are taken
//! in arrival order), windows that forget old data, and a transport. State
//! can be saved and restored with [`Live::checkpoint`] and [`Live::restore`].

use crate::{cohesion, hash_of};
use kute_types::{KuteError, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// The folded symbol: every symbol past `max_symbols`, or too long.
const OTHER: u32 = 0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveParams {
    /// Longest sequence counted, in symbols (2 to 16).
    pub max_length: usize,
    /// Distinct symbols known by name. Later ones are folded together.
    pub max_symbols: usize,
    /// A symbol longer than this many bytes is folded as well.
    pub max_symbol_bytes: usize,
    /// Sequences counted at each length.
    pub max_candidates: usize,
    /// Entities whose recent symbols are remembered.
    pub max_entities: usize,
}

impl Default for LiveParams {
    fn default() -> Self {
        Self { max_length: 6, max_symbols: 65_536, max_symbol_bytes: 256, max_candidates: 200_000, max_entities: 100_000 }
    }
}

impl LiveParams {
    pub fn validate(&self) -> Result<()> {
        if !(2..=16).contains(&self.max_length) {
            return Err(KuteError::InvalidConfig("max_length must be between 2 and 16".into()));
        }
        if self.max_symbols == 0 || self.max_candidates == 0 || self.max_entities == 0 || self.max_symbol_bytes == 0 {
            return Err(KuteError::InvalidConfig("every bound must be at least 1".into()));
        }
        Ok(())
    }

    /// An upper estimate of the bytes of state these bounds allow: names,
    /// counted sequences and remembered tails, with the tables' overhead.
    /// The point is that it is a function of the bounds and not of the
    /// stream.
    pub fn state_bytes_bound(&self) -> u64 {
        const MAP_ENTRY: u64 = 48; // key pointer and length, value, hash-table slack
        let names = self.max_symbols as u64 * (2 * self.max_symbol_bytes as u64 + 2 * MAP_ENTRY);
        let sequences: u64 = (1..=self.max_length as u64).map(|n| self.max_candidates as u64 * (4 * n + MAP_ENTRY + 8)).sum();
        let tails = self.max_entities as u64 * (4 * self.max_length as u64 + MAP_ENTRY + 24);
        names + sequences + tails
    }
}

#[derive(Clone, Debug, Default)]
struct Table {
    counts: FxHashMap<Box<[u32]>, u64>,
    /// Sequences of this length offered.
    offered: u64,
    /// Times every count was lowered by one: the most any count is under.
    decrements: u64,
}

impl Table {
    /// Misra-Gries: a new sequence when the table is full lowers every
    /// count by one, and is itself dropped. Each such step removes
    /// `capacity + 1` from the total held, so there are at most
    /// `offered / (capacity + 1)` of them and the work is O(1) amortized.
    fn offer(&mut self, sequence: &[u32], capacity: usize) {
        self.offered += 1;
        if let Some(count) = self.counts.get_mut(sequence) {
            *count += 1;
        } else if self.counts.len() < capacity {
            self.counts.insert(sequence.into(), 1);
        } else {
            self.decrements += 1;
            self.counts.retain(|_, count| {
                *count -= 1;
                *count > 0
            });
        }
    }
}

#[derive(Clone, Debug)]
struct Tail {
    recent: VecDeque<u32>,
    last_seen: u64,
}

/// A pattern found in the stream so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LivePattern {
    pub symbols: Vec<String>,
    /// Occurrences counted: never more than the true number.
    pub support: u64,
    /// At most this many reached the count. Occurrences cut by an evicted
    /// entity or a folded symbol are in neither number.
    pub support_at_most: u64,
    /// As [`crate::Pattern::cohesion_bits`], from the counts held.
    pub cohesion_bits: f64,
    pub evidence_bits: f64,
    pub content_hash: String,
}

/// What the stream has shown so far, and how far the bounds were pressed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveReport {
    pub events: u64,
    pub alphabet: u64,
    /// True when no bound was reached: the counts are exact.
    pub exact: bool,
    /// Events whose symbol was folded into "other".
    pub events_folded: u64,
    pub entities_tracked: u64,
    /// Entities whose recent symbols were dropped to make room.
    pub entities_evicted: u64,
    /// For lengths 1 to `max_length`: sequences held.
    pub sequences_held: Vec<u64>,
    /// For the same lengths: the most a reported support is under the truth.
    pub max_undercount: Vec<u64>,
    /// Bytes of state in use, by the same accounting as
    /// [`LiveParams::state_bytes_bound`].
    pub state_bytes: u64,
    pub state_bytes_bound: u64,
    pub patterns: Vec<LivePattern>,
}

#[derive(Clone, Debug)]
pub struct Live {
    params: LiveParams,
    names: Vec<String>,
    index: FxHashMap<String, u32>,
    /// `tables[n]` counts sequences of length `n`; `tables[0]` is unused.
    tables: Vec<Table>,
    entities: FxHashMap<u64, Tail>,
    events: u64,
    folded: u64,
    evicted: u64,
}

/// FNV-1a: fixed, so a checkpoint means the same on every build. Two
/// entities with the same hash share a tail; with 64 bits that is rare, and
/// it is one more way an entity's context can be wrong, not a new kind.
fn entity_key(entity: &str) -> u64 {
    entity.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

impl Live {
    pub fn new(params: LiveParams) -> Result<Self> {
        params.validate()?;
        let tables = vec![Table::default(); params.max_length + 1];
        Ok(Self { params, names: vec![String::new()], index: FxHashMap::default(), tables, entities: FxHashMap::default(), events: 0, folded: 0, evicted: 0 })
    }

    pub fn params(&self) -> &LiveParams {
        &self.params
    }

    fn symbol_id(&mut self, symbol: &str) -> u32 {
        if let Some(&id) = self.index.get(symbol) {
            return id;
        }
        if symbol.is_empty() || symbol.len() > self.params.max_symbol_bytes || self.index.len() >= self.params.max_symbols {
            return OTHER;
        }
        let id = self.names.len() as u32;
        self.names.push(symbol.to_string());
        self.index.insert(symbol.to_string(), id);
        id
    }

    /// Drop the least recently seen eighth of the entities.
    fn evict(&mut self) {
        let mut seen: Vec<u64> = self.entities.values().map(|t| t.last_seen).collect();
        let cut = seen.len() / 8;
        let (_, threshold, _) = seen.select_nth_unstable(cut);
        let threshold = *threshold;
        let before = self.entities.len();
        self.entities.retain(|_, tail| tail.last_seen > threshold);
        self.evicted += (before - self.entities.len()) as u64;
    }

    /// Take one event: `entity` produced `symbol`. Events of one entity
    /// form one sequence, in the order they arrive.
    pub fn observe(&mut self, entity: &str, symbol: &str) {
        self.events += 1;
        let id = self.symbol_id(symbol);
        if id == OTHER {
            self.folded += 1;
        }
        let key = entity_key(entity);
        if !self.entities.contains_key(&key) && self.entities.len() >= self.params.max_entities {
            self.evict();
        }
        let max_length = self.params.max_length;
        let tail = self.entities.entry(key).or_insert_with(|| Tail { recent: VecDeque::with_capacity(max_length), last_seen: 0 });
        tail.last_seen = self.events;
        if tail.recent.len() == max_length {
            tail.recent.pop_front();
        }
        tail.recent.push_back(id);
        // Every sequence that ends at this event, shortest first; one that
        // runs through a folded symbol is not counted, nor any longer one.
        let recent = tail.recent.make_contiguous();
        for n in 1..=recent.len() {
            let sequence = &recent[recent.len() - n..];
            if sequence[0] == OTHER {
                break;
            }
            self.tables[n].offer(sequence, self.params.max_candidates);
        }
    }

    /// End an entity's sequence: its next event starts a new one.
    pub fn close(&mut self, entity: &str) {
        self.entities.remove(&entity_key(entity));
    }

    fn state_bytes(&self) -> u64 {
        const MAP_ENTRY: u64 = 48;
        let names: u64 = self.names.iter().map(|n| 2 * n.len() as u64 + 2 * MAP_ENTRY).sum();
        let sequences: u64 = self.tables.iter().enumerate().map(|(n, t)| t.counts.len() as u64 * (4 * n as u64 + MAP_ENTRY + 8)).sum();
        let tails = self.entities.len() as u64 * (4 * self.params.max_length as u64 + MAP_ENTRY + 24);
        names + sequences + tails
    }

    /// The patterns found so far: sequences of two or more symbols counted
    /// at least `min_support` times with at least `min_cohesion_bits`,
    /// strongest first, at most `max_patterns`.
    pub fn report(&self, min_support: u64, min_cohesion_bits: f64, max_patterns: usize) -> LiveReport {
        let total = self.tables[1].offered;
        let count_of = |seq: &[u32]| self.tables.get(seq.len()).and_then(|t| t.counts.get(seq)).copied().unwrap_or(0);
        let mut patterns = Vec::new();
        for n in 2..self.tables.len() {
            // As in `discover`: a sequence a longer one extends, occurring
            // exactly as often, is left to the longer one.
            let mut absorbed: FxHashSet<&[u32]> = FxHashSet::default();
            if let Some(longer) = self.tables.get(n + 1) {
                for (seq, &count) in longer.counts.iter().filter(|(_, &c)| c >= min_support) {
                    for part in [&seq[..n], &seq[1..]] {
                        if count_of(part) == count {
                            absorbed.insert(part);
                        }
                    }
                }
            }
            for (seq, &count) in &self.tables[n].counts {
                if count < min_support || absorbed.contains(&seq[..]) {
                    continue;
                }
                let Some(bits) = cohesion(seq, count, total, &count_of) else { continue };
                if bits < min_cohesion_bits {
                    continue;
                }
                let symbols: Vec<String> = seq.iter().map(|&id| self.names[id as usize].clone()).collect();
                patterns.push(LivePattern {
                    content_hash: hash_of(&symbols),
                    symbols,
                    support: count,
                    support_at_most: count + self.tables[n].decrements,
                    cohesion_bits: bits,
                    evidence_bits: count as f64 * bits,
                });
            }
        }
        patterns.sort_by(|a, b| b.evidence_bits.total_cmp(&a.evidence_bits).then_with(|| b.symbols.len().cmp(&a.symbols.len())).then_with(|| a.symbols.cmp(&b.symbols)));
        patterns.truncate(max_patterns);
        let max_undercount: Vec<u64> = self.tables[1..].iter().map(|t| t.decrements).collect();
        LiveReport {
            events: self.events,
            alphabet: self.index.len() as u64,
            exact: self.folded == 0 && self.evicted == 0 && max_undercount.iter().all(|&d| d == 0),
            events_folded: self.folded,
            entities_tracked: self.entities.len() as u64,
            entities_evicted: self.evicted,
            sequences_held: self.tables[1..].iter().map(|t| t.counts.len() as u64).collect(),
            max_undercount,
            state_bytes: self.state_bytes(),
            state_bytes_bound: self.params.state_bytes_bound(),
            patterns,
        }
    }

    /// Everything needed to go on after a restart, as JSON. Deterministic:
    /// the same state gives the same bytes.
    pub fn checkpoint(&self) -> Result<Vec<u8>> {
        let mut entities: Vec<(u64, Vec<u32>, u64)> = self.entities.iter().map(|(&k, t)| (k, t.recent.iter().copied().collect(), t.last_seen)).collect();
        entities.sort_unstable();
        let tables = self.tables[1..]
            .iter()
            .map(|t| {
                let mut counts: Vec<(Vec<u32>, u64)> = t.counts.iter().map(|(k, &c)| (k.to_vec(), c)).collect();
                counts.sort_unstable();
                SavedTable { counts, offered: t.offered, decrements: t.decrements }
            })
            .collect();
        let saved = Saved { format: SAVED_FORMAT, params: self.params.clone(), names: self.names[1..].to_vec(), tables, entities, events: self.events, folded: self.folded, evicted: self.evicted };
        serde_json::to_vec(&saved).map_err(|e| KuteError::InvalidInput(format!("checkpoint: {e}")))
    }

    /// Rebuild from [`Live::checkpoint`]. The bytes are checked against the
    /// bounds they declare, so a damaged or hostile checkpoint cannot make
    /// a state larger than its own parameters allow.
    pub fn restore(bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| KuteError::InvalidInput(format!("not a usable checkpoint: {why}"));
        let saved: Saved = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
        if saved.format != SAVED_FORMAT {
            return Err(bad("unknown format"));
        }
        saved.params.validate()?;
        let p = &saved.params;
        if saved.names.len() > p.max_symbols || saved.names.iter().any(|n| n.is_empty() || n.len() > p.max_symbol_bytes) {
            return Err(bad("more or longer symbols than its bounds allow"));
        }
        if saved.tables.len() != p.max_length || saved.entities.len() > p.max_entities {
            return Err(bad("tables or entities outside its bounds"));
        }
        // The numbers must be ones a stream could have produced. Otherwise
        // a count near the top of the range overflows on the next event or
        // in the next report.
        if saved.events > MAX_EVENTS || saved.folded > saved.events {
            return Err(bad("more events than a stream can have, or more folded than events"));
        }
        let known = saved.names.len() as u32;
        let mut live = Live::new(saved.params.clone())?;
        for name in saved.names {
            if live.index.insert(name.clone(), live.names.len() as u32).is_some() {
                return Err(bad("a symbol is listed twice"));
            }
            live.names.push(name);
        }
        for (i, table) in saved.tables.into_iter().enumerate() {
            let n = i + 1;
            if table.counts.len() > p.max_candidates {
                return Err(bad("more sequences than its bounds allow"));
            }
            // Every event offers at most one sequence of each length; each
            // lowering of the counts answers `max_candidates + 1` offers;
            // and what is held cannot exceed what was offered.
            let per_decrement = (p.max_candidates as u64).saturating_add(1);
            if table.offered > saved.events || table.decrements > table.offered / per_decrement {
                return Err(bad("a table was offered or lowered more than the events allow"));
            }
            let mut held: u64 = 0;
            for (_, count) in &table.counts {
                held = held.checked_add(*count).filter(|&h| h <= table.offered).ok_or_else(|| bad("a table holds more than it was offered"))?;
            }
            let target = &mut live.tables[n];
            for (seq, count) in table.counts {
                if seq.len() != n || count == 0 || seq.iter().any(|&id| id == OTHER || id > known) {
                    return Err(bad("a counted sequence is malformed"));
                }
                if target.counts.insert(seq.into(), count).is_some() {
                    return Err(bad("a sequence is counted twice"));
                }
            }
            target.offered = table.offered;
            target.decrements = table.decrements;
        }
        for (key, recent, last_seen) in saved.entities {
            if recent.len() > p.max_length || recent.iter().any(|&id| id > known) || last_seen > saved.events {
                return Err(bad("an entity's recent symbols are malformed"));
            }
            if live.entities.insert(key, Tail { recent: recent.into(), last_seen }).is_some() {
                return Err(bad("an entity is listed twice"));
            }
        }
        live.events = saved.events;
        live.folded = saved.folded;
        live.evicted = saved.evicted;
        Ok(live)
    }
}

const SAVED_FORMAT: u32 = 1;
/// More events than any stream delivers (a million a second for 146,000
/// years). A checkpoint that claims more is damaged; refusing it keeps
/// every sum of counts inside 64 bits.
const MAX_EVENTS: u64 = 1 << 62;

#[derive(Serialize, Deserialize)]
struct SavedTable {
    counts: Vec<(Vec<u32>, u64)>,
    offered: u64,
    decrements: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    format: u32,
    params: LiveParams,
    names: Vec<String>,
    tables: Vec<SavedTable>,
    entities: Vec<(u64, Vec<u32>, u64)>,
    events: u64,
    folded: u64,
    evicted: u64,
}
