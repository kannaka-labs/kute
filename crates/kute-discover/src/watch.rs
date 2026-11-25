//! Live discovery by event time, in windows that forget.
//!
//! [`Watch`] takes events as they arrive, each with an entity, a symbol
//! and a time, and reports the recurring sequences of each window of
//! event time. It is [`crate::live::Live`] with three things added.
//!
//! **Order.** Events are counted in order of their time, not of their
//! arrival. Arrivals wait in a buffer until the watermark passes them; the
//! watermark is the latest time seen minus `lateness`. Events with equal
//! times are counted in the order they arrived, so the same arrivals
//! always give the same result. An event that arrives with a time before
//! the watermark is **late**: it is dropped and counted as dropped. When
//! the buffer is full its earliest event is released early and the
//! watermark jumps to that event's time (a **forced release**, counted),
//! which can make later arrivals late.
//!
//! **Windows.** Time is cut into panes of `pane_ticks`. A window is the
//! last `panes` panes: one pane gives tumbling windows, several give a
//! sliding window that moves a pane at a time. Each pane has its own
//! counts, and a pane older than the window is discarded whole, which is
//! how old data is forgotten. A report is produced when a pane that
//! received events closes, that is, when the first event of a later pane
//! is released. Stretches of time without events produce no reports.
//!
//! A sequence is counted in the pane where its last event falls. Entities'
//! recent symbols are kept across panes, so a sequence that crosses a
//! pane boundary is counted, once, in the later pane.
//!
//! **Change.** Each report lists the patterns that were not in the report
//! before it and those that were and no longer are.
//!
//! The bounds and approximations of `Live` apply per pane: counts are
//! Misra-Gries summaries, symbols past the limit are folded, entities past
//! the limit lose their recent symbols. Across the panes of a window the
//! counts add and so do the undercount bounds. A report says `exact` only
//! if nothing in its window was approximated, dropped late or released
//! early.
//!
//! Not here: durations of patterns, gapped pairs, windows by event count,
//! and any transport. Times are whole numbers of a unit the caller
//! chooses.

use crate::live::{entity_key, patterns_of, LiveParams, LivePattern, Table, Tail, OTHER};
use kute_types::{KuteError, Result};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

/// More events than a watch will count: a million a second for 35 years.
/// Keeps every sum over the panes of a window inside 64 bits.
const MAX_EVENTS: u64 = 1 << 50;
/// Most panes in a window.
pub const MAX_PANES: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WatchParams {
    /// The bounds of each pane's counting.
    #[serde(flatten)]
    pub live: LiveParams,
    /// Length of a pane in ticks of the caller's time unit.
    pub pane_ticks: i64,
    /// Panes in a window (1 to 64).
    pub panes: usize,
    /// How far behind the latest time an event may arrive and still be counted.
    pub lateness: i64,
    /// Events held waiting for the watermark.
    pub max_buffered: usize,
    /// What a sequence needs to be reported, as in [`crate::live::Live::report`].
    pub min_support: u64,
    pub min_cohesion_bits: f64,
    pub max_patterns: usize,
}

impl Default for WatchParams {
    fn default() -> Self {
        Self { live: LiveParams::default(), pane_ticks: 3600, panes: 1, lateness: 0, max_buffered: 100_000, min_support: 5, min_cohesion_bits: 1.0, max_patterns: 100 }
    }
}

impl WatchParams {
    pub fn validate(&self) -> Result<()> {
        self.live.validate()?;
        let bad = |why: &str| Err(KuteError::InvalidConfig(why.into()));
        if self.pane_ticks <= 0 {
            return bad("pane_ticks must be positive");
        }
        if !(1..=MAX_PANES).contains(&self.panes) {
            return bad("panes must be between 1 and 64");
        }
        if self.lateness < 0 {
            return bad("lateness cannot be negative");
        }
        if self.max_buffered == 0 || self.max_buffered > 1 << 28 {
            return bad("max_buffered must be between 1 and 2^28");
        }
        if !self.min_cohesion_bits.is_finite() || self.max_patterns > 100_000 {
            return bad("min_cohesion_bits must be finite and max_patterns at most 100,000");
        }
        Ok(())
    }

    /// An upper estimate of the bytes of state these bounds allow. A
    /// function of the bounds, not of the stream. With more than one pane
    /// it includes the merged copy of the counts a report builds.
    pub fn state_bytes_bound(&self) -> u64 {
        const MAP_ENTRY: u64 = 48;
        let l = &self.live;
        let names = l.max_symbols as u64 * (2 * l.max_symbol_bytes as u64 + 2 * MAP_ENTRY);
        let sequences: u64 = (1..=l.max_length as u64).map(|n| l.max_candidates as u64 * (4 * n + MAP_ENTRY + 8)).sum();
        let copies = if self.panes > 1 { 2 * self.panes as u64 } else { 1 };
        let tails = l.max_entities as u64 * (4 * l.max_length as u64 + MAP_ENTRY + 24);
        let buffer = self.max_buffered as u64 * 40;
        let previous = self.max_patterns as u64 * (96 + l.max_length as u64 * (l.max_symbol_bytes as u64 + 24));
        names + copies * sequences + tails + buffer + previous
    }
}

#[derive(Clone, Debug)]
struct Pane {
    index: i64,
    /// `tables[n]` counts sequences of length `n`; `tables[0]` is unused.
    tables: Vec<Table>,
    events: u64,
    folded: u64,
    late: u64,
    forced: u64,
    evicted: u64,
}

impl Pane {
    fn new(index: i64, max_length: usize) -> Self {
        Self { index, tables: vec![Table::default(); max_length + 1], events: 0, folded: 0, late: 0, forced: 0, evicted: 0 }
    }
}

/// What one window held.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowReport {
    /// The window covers times from `start` up to but not including `end`.
    pub start: i64,
    pub end: i64,
    /// True for the report [`Watch::finish`] gives for the pane that was
    /// still open: the stream ended, no later event closed it.
    pub at_end: bool,
    /// Events counted in the window.
    pub events: u64,
    /// Distinct symbols known by name, over the whole stream so far.
    pub alphabet: u64,
    /// Nothing in this window was approximated, dropped or released early.
    pub exact: bool,
    pub events_folded: u64,
    /// Events dropped for arriving behind the watermark while this window's panes were open.
    pub late_dropped: u64,
    /// Events released before the watermark reached them because the buffer was full.
    pub forced_releases: u64,
    pub entities_tracked: u64,
    pub entities_evicted: u64,
    /// Sequences held, by length from one, summed over the panes.
    pub sequences_held: Vec<u64>,
    /// The most a count of each length can be under.
    pub max_undercount: Vec<u64>,
    pub state_bytes: u64,
    pub state_bytes_bound: u64,
    pub patterns: Vec<LivePattern>,
    /// Start of the window the next two lists compare with; absent on the first report.
    pub previous_start: Option<i64>,
    /// Content hashes of patterns here that the previous report did not have.
    pub appeared: Vec<String>,
    /// Patterns the previous report had and this one does not.
    pub vanished: Vec<Vec<String>>,
}

/// Totals since the watch began, for a caller's own accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Events given to [`Watch::observe`], counted or not.
    pub arrived: u64,
    /// Events counted in some pane.
    pub counted: u64,
    pub late_dropped: u64,
    pub forced_releases: u64,
    pub reports: u64,
}

type Held = Reverse<(i64, u64, u64, u32)>;
/// The last report's window start and its patterns, as (content hash, symbols).
type Previous = (i64, Vec<(String, Vec<String>)>);

#[derive(Clone, Debug)]
pub struct Watch {
    params: WatchParams,
    names: Vec<String>,
    index: FxHashMap<String, u32>,
    tails: FxHashMap<u64, Tail>,
    /// Time, arrival number, entity key, symbol.
    buffer: BinaryHeap<Held>,
    watermark: Option<i64>,
    latest: Option<i64>,
    /// The panes of the current window, oldest first; the last is open.
    panes: VecDeque<Pane>,
    /// Late, forced and evicted since the open pane began.
    pending: (u64, u64, u64),
    previous: Option<Previous>,
    totals: Totals,
}

impl Watch {
    pub fn new(params: WatchParams) -> Result<Self> {
        params.validate()?;
        Ok(Self {
            params,
            names: vec![String::new()],
            index: FxHashMap::default(),
            tails: FxHashMap::default(),
            buffer: BinaryHeap::new(),
            watermark: None,
            latest: None,
            panes: VecDeque::new(),
            pending: (0, 0, 0),
            previous: None,
            totals: Totals::default(),
        })
    }

    pub fn params(&self) -> &WatchParams {
        &self.params
    }

    pub fn totals(&self) -> Totals {
        self.totals
    }

    /// Events waiting for the watermark.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    fn symbol_id(&mut self, symbol: &str) -> u32 {
        if let Some(&id) = self.index.get(symbol) {
            return id;
        }
        let l = &self.params.live;
        if symbol.is_empty() || symbol.len() > l.max_symbol_bytes || self.index.len() >= l.max_symbols {
            return OTHER;
        }
        let id = self.names.len() as u32;
        self.names.push(symbol.to_string());
        self.index.insert(symbol.to_string(), id);
        id
    }

    /// Take one event. Returns the reports of the windows it closed,
    /// usually none.
    pub fn observe(&mut self, entity: &str, symbol: &str, time: i64) -> Vec<WindowReport> {
        let mut out = Vec::new();
        self.totals.arrived += 1;
        if self.watermark.is_some_and(|mark| time < mark) {
            self.totals.late_dropped += 1;
            self.pending.0 += 1;
            return out;
        }
        let id = self.symbol_id(symbol);
        self.buffer.push(Reverse((time, self.totals.arrived, entity_key(entity), id)));
        let latest = self.latest.map_or(time, |t| t.max(time));
        self.latest = Some(latest);
        let mark = latest.saturating_sub(self.params.lateness);
        let mut mark = self.watermark.map_or(mark, |w| w.max(mark));
        while let Some(&Reverse((t, _, key, id))) = self.buffer.peek() {
            if t > mark && self.buffer.len() <= self.params.max_buffered {
                break;
            }
            self.buffer.pop();
            if t > mark {
                mark = t;
                self.totals.forced_releases += 1;
                self.pending.1 += 1;
            }
            self.release(t, key, id, &mut out);
        }
        self.watermark = Some(mark);
        out
    }

    /// Count one event, in time order.
    fn release(&mut self, time: i64, key: u64, id: u32, out: &mut Vec<WindowReport>) {
        let q = time.div_euclid(self.params.pane_ticks);
        if self.panes.back().is_none_or(|open| open.index != q) {
            if !self.panes.is_empty() {
                out.push(self.report(false));
                let oldest_kept = q as i128 - self.params.panes as i128;
                self.panes.retain(|pane| pane.index as i128 > oldest_kept);
            }
            self.panes.push_back(Pane::new(q, self.params.live.max_length));
        }
        self.totals.counted += 1;
        let l = &self.params.live;
        let (max_length, max_candidates, max_entities) = (l.max_length, l.max_candidates, l.max_entities);
        if !self.tails.contains_key(&key) && self.tails.len() >= max_entities {
            // Drop the least recently seen eighth.
            let mut seen: Vec<u64> = self.tails.values().map(|t| t.last_seen).collect();
            let cut = seen.len() / 8;
            let threshold = *seen.select_nth_unstable(cut).1;
            let before = self.tails.len();
            self.tails.retain(|_, tail| tail.last_seen > threshold);
            self.pending.2 += (before - self.tails.len()) as u64;
        }
        let tail = self.tails.entry(key).or_insert_with(|| Tail { recent: VecDeque::with_capacity(max_length), last_seen: 0 });
        tail.last_seen = self.totals.counted;
        if tail.recent.len() == max_length {
            tail.recent.pop_front();
        }
        tail.recent.push_back(id);
        let pane = self.panes.back_mut().expect("a pane is open");
        pane.events += 1;
        if id == OTHER {
            pane.folded += 1;
        }
        let recent = tail.recent.make_contiguous();
        for n in 1..=recent.len() {
            let sequence = &recent[recent.len() - n..];
            if sequence[0] == OTHER {
                break;
            }
            pane.tables[n].offer(sequence, max_candidates);
        }
    }

    /// End an entity's sequence now: its next counted event starts a new
    /// one. Takes effect at once, whatever is still buffered.
    pub fn close(&mut self, entity: &str) {
        self.tails.remove(&entity_key(entity));
    }

    /// The stream has ended: count what was still buffered and report the
    /// open window.
    pub fn finish(mut self) -> (Vec<WindowReport>, Totals) {
        let mut out = Vec::new();
        while let Some(Reverse((t, _, key, id))) = self.buffer.pop() {
            self.release(t, key, id, &mut out);
        }
        if !self.panes.is_empty() {
            out.push(self.report(true));
        }
        (out, self.totals)
    }

    fn state_bytes(&self) -> u64 {
        const MAP_ENTRY: u64 = 48;
        let names: u64 = self.names.iter().map(|n| 2 * n.len() as u64 + 2 * MAP_ENTRY).sum();
        let sequences: u64 = self.panes.iter().flat_map(|p| p.tables.iter().enumerate()).map(|(n, t)| t.counts.len() as u64 * (4 * n as u64 + MAP_ENTRY + 8)).sum();
        let tails = self.tails.len() as u64 * (4 * self.params.live.max_length as u64 + MAP_ENTRY + 24);
        let previous: u64 = self.previous.iter().flat_map(|(_, p)| p.iter()).map(|(_, symbols)| 96 + symbols.iter().map(|s| s.len() as u64 + 24).sum::<u64>()).sum();
        names + sequences + tails + self.buffer.len() as u64 * 40 + previous
    }

    /// Report the window that ends with the open pane.
    fn report(&mut self, at_end: bool) -> WindowReport {
        let (late, forced, evicted) = std::mem::take(&mut self.pending);
        let open = self.panes.back_mut().expect("a pane is open");
        open.late += late;
        open.forced += forced;
        open.evicted += evicted;
        let last = open.index;
        let state_bytes = self.state_bytes();
        let merged: Vec<Table>;
        let tables: &[Table] = if self.panes.len() == 1 {
            &self.panes[0].tables
        } else {
            let mut sum = vec![Table::default(); self.params.live.max_length + 1];
            for pane in &self.panes {
                for (into, from) in sum.iter_mut().zip(&pane.tables) {
                    into.offered += from.offered;
                    into.decrements += from.decrements;
                    for (sequence, count) in &from.counts {
                        *into.counts.entry(sequence.clone()).or_insert(0) += count;
                    }
                }
            }
            merged = sum;
            &merged
        };
        let p = &self.params;
        let patterns = patterns_of(tables, &self.names, p.min_support, p.min_cohesion_bits, p.max_patterns);
        let max_undercount: Vec<u64> = tables[1..].iter().map(|t| t.decrements).collect();
        let sum = |f: fn(&Pane) -> u64| self.panes.iter().map(f).sum::<u64>();
        let (events_folded, late_dropped, forced_releases, entities_evicted) = (sum(|p| p.folded), sum(|p| p.late), sum(|p| p.forced), sum(|p| p.evicted));
        let clamp = |v: i128| v.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        let start = clamp((last as i128 - p.panes as i128 + 1) * p.pane_ticks as i128);
        let end = clamp((last as i128 + 1) * p.pane_ticks as i128);
        let now: Vec<(String, Vec<String>)> = patterns.iter().map(|x| (x.content_hash.clone(), x.symbols.clone())).collect();
        let (previous_start, appeared, vanished) = match &self.previous {
            None => (None, Vec::new(), Vec::new()),
            Some((at, before)) => {
                let had: rustc_hash::FxHashSet<&str> = before.iter().map(|(h, _)| h.as_str()).collect();
                let has: rustc_hash::FxHashSet<&str> = now.iter().map(|(h, _)| h.as_str()).collect();
                (
                    Some(*at),
                    now.iter().filter(|(h, _)| !had.contains(h.as_str())).map(|(h, _)| h.clone()).collect(),
                    before.iter().filter(|(h, _)| !has.contains(h.as_str())).map(|(_, s)| s.clone()).collect(),
                )
            }
        };
        let report = WindowReport {
            start,
            end,
            at_end,
            events: sum(|p| p.events),
            alphabet: self.index.len() as u64,
            exact: events_folded == 0 && late_dropped == 0 && forced_releases == 0 && entities_evicted == 0 && max_undercount.iter().all(|&d| d == 0),
            events_folded,
            late_dropped,
            forced_releases,
            entities_tracked: self.tails.len() as u64,
            entities_evicted,
            sequences_held: tables[1..].iter().map(|t| t.counts.len() as u64).collect(),
            max_undercount,
            state_bytes,
            state_bytes_bound: p.state_bytes_bound(),
            patterns,
            previous_start,
            appeared,
            vanished,
        };
        self.previous = Some((start, now));
        self.totals.reports += 1;
        report
    }

    /// Everything needed to go on after a restart, as JSON. Deterministic:
    /// the same state gives the same bytes.
    pub fn checkpoint(&self) -> Result<Vec<u8>> {
        let mut tails: Vec<(u64, Vec<u32>, u64)> = self.tails.iter().map(|(&k, t)| (k, t.recent.iter().copied().collect(), t.last_seen)).collect();
        tails.sort_unstable();
        let mut buffer: Vec<(i64, u64, u64, u32)> = self.buffer.iter().map(|held| held.0).collect();
        buffer.sort_unstable();
        let panes = self
            .panes
            .iter()
            .map(|pane| SavedPane {
                index: pane.index,
                tables: pane.tables[1..]
                    .iter()
                    .map(|t| {
                        let mut counts: Vec<(Vec<u32>, u64)> = t.counts.iter().map(|(k, &c)| (k.to_vec(), c)).collect();
                        counts.sort_unstable();
                        SavedTable { counts, offered: t.offered, decrements: t.decrements }
                    })
                    .collect(),
                events: pane.events,
                folded: pane.folded,
                late: pane.late,
                forced: pane.forced,
                evicted: pane.evicted,
            })
            .collect();
        let saved = Saved {
            format: SAVED_FORMAT,
            params: self.params.clone(),
            names: self.names[1..].to_vec(),
            tails,
            buffer,
            watermark: self.watermark,
            latest: self.latest,
            panes,
            pending: self.pending,
            previous: self.previous.clone(),
            totals: self.totals,
        };
        serde_json::to_vec(&saved).map_err(|e| KuteError::InvalidInput(format!("checkpoint: {e}")))
    }

    /// Rebuild from [`Watch::checkpoint`]. The bytes are checked against
    /// the bounds they declare, so a damaged or hostile checkpoint cannot
    /// make a state larger than its own parameters allow, or one whose
    /// counts overflow later.
    pub fn restore(bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| KuteError::InvalidInput(format!("not a usable checkpoint: {why}"));
        let saved: Saved = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
        if saved.format != SAVED_FORMAT {
            return Err(bad("unknown format"));
        }
        let mut watch = Watch::new(saved.params)?;
        let p = watch.params.clone();
        let l = &p.live;
        if saved.names.len() > l.max_symbols || saved.names.iter().any(|n| n.is_empty() || n.len() > l.max_symbol_bytes) {
            return Err(bad("more or longer symbols than its bounds allow"));
        }
        let known = saved.names.len() as u32;
        for name in saved.names {
            if watch.index.insert(name.clone(), watch.names.len() as u32).is_some() {
                return Err(bad("a symbol is listed twice"));
            }
            watch.names.push(name);
        }
        let t = saved.totals;
        if t.arrived > MAX_EVENTS || t.counted > t.arrived || t.late_dropped > t.arrived || t.forced_releases > t.arrived || t.reports > t.arrived {
            return Err(bad("totals no stream could produce"));
        }
        if saved.pending.0 > t.arrived || saved.pending.1 > t.arrived || saved.pending.2 > t.arrived {
            return Err(bad("pending counts above the events"));
        }
        if saved.tails.len() > l.max_entities || saved.buffer.len() > p.max_buffered || saved.panes.len() > p.panes {
            return Err(bad("entities, buffer or panes outside its bounds"));
        }
        match (saved.watermark, saved.latest) {
            (None, None) if saved.buffer.is_empty() && saved.panes.is_empty() && t.arrived == 0 => {}
            (Some(mark), Some(latest)) if mark <= latest => {}
            _ => return Err(bad("watermark and latest time disagree")),
        }
        for &(time, arrival, _, id) in &saved.buffer {
            if id > known || arrival == 0 || arrival > t.arrived || saved.watermark.is_some_and(|mark| time < mark) || saved.latest.is_some_and(|latest| time > latest) {
                return Err(bad("a buffered event is malformed"));
            }
        }
        let mut arrivals: Vec<u64> = saved.buffer.iter().map(|b| b.1).collect();
        arrivals.sort_unstable();
        if arrivals.windows(2).any(|w| w[0] == w[1]) {
            return Err(bad("two buffered events share an arrival number"));
        }
        watch.buffer = saved.buffer.into_iter().map(Reverse).collect();
        for (key, recent, last_seen) in saved.tails {
            if recent.len() > l.max_length || recent.iter().any(|&id| id > known) || last_seen > t.counted {
                return Err(bad("an entity's recent symbols are malformed"));
            }
            if watch.tails.insert(key, Tail { recent: recent.into(), last_seen }).is_some() {
                return Err(bad("an entity is listed twice"));
            }
        }
        let mut counted: u64 = 0;
        let mut last_index: Option<i64> = None;
        let per_decrement = (l.max_candidates as u64).saturating_add(1);
        for saved_pane in saved.panes {
            if last_index.is_some_and(|i| saved_pane.index <= i) || saved_pane.tables.len() != l.max_length {
                return Err(bad("panes out of order or of the wrong shape"));
            }
            last_index = Some(saved_pane.index);
            counted = counted.checked_add(saved_pane.events).filter(|&c| c <= t.counted).ok_or_else(|| bad("panes hold more events than were counted"))?;
            if saved_pane.folded > saved_pane.events || saved_pane.late > t.arrived || saved_pane.forced > t.arrived || saved_pane.evicted > t.arrived {
                return Err(bad("a pane's counters exceed its events"));
            }
            let mut pane = Pane::new(saved_pane.index, l.max_length);
            for (i, table) in saved_pane.tables.into_iter().enumerate() {
                let n = i + 1;
                if table.counts.len() > l.max_candidates || table.offered > saved_pane.events || table.decrements > table.offered / per_decrement {
                    return Err(bad("a table is outside what its pane's events allow"));
                }
                let mut held: u64 = 0;
                let target = &mut pane.tables[n];
                for (sequence, count) in table.counts {
                    held = held.checked_add(count).filter(|&h| h <= table.offered).ok_or_else(|| bad("a table holds more than it was offered"))?;
                    if sequence.len() != n || count == 0 || sequence.iter().any(|&id| id == OTHER || id > known) {
                        return Err(bad("a counted sequence is malformed"));
                    }
                    if target.counts.insert(sequence.into(), count).is_some() {
                        return Err(bad("a sequence is counted twice"));
                    }
                }
                target.offered = table.offered;
                target.decrements = table.decrements;
            }
            pane.events = saved_pane.events;
            pane.folded = saved_pane.folded;
            pane.late = saved_pane.late;
            pane.forced = saved_pane.forced;
            pane.evicted = saved_pane.evicted;
            watch.panes.push_back(pane);
        }
        if let (Some(first), Some(last)) = (watch.panes.front(), watch.panes.back()) {
            if last.index as i128 - first.index as i128 >= p.panes as i128 {
                return Err(bad("panes span more than a window"));
            }
        }
        // Every counted event was at or before the watermark, so no pane
        // can lie beyond it. One that did would never be the oldest, would
        // never be discarded, and its events would be in every window after.
        if let (Some(last), Some(mark)) = (watch.panes.back(), saved.watermark) {
            if last.index > mark.div_euclid(p.pane_ticks) {
                return Err(bad("a pane lies after the watermark"));
            }
        }
        if let Some((_, patterns)) = &saved.previous {
            if patterns.len() > p.max_patterns || patterns.iter().any(|(hash, symbols)| hash.len() > 96 || symbols.len() > l.max_length || symbols.iter().any(|s| s.len() > l.max_symbol_bytes)) {
                return Err(bad("the previous report is outside its bounds"));
            }
        }
        watch.previous = saved.previous;
        watch.watermark = saved.watermark;
        watch.latest = saved.latest;
        watch.pending = saved.pending;
        watch.totals = saved.totals;
        Ok(watch)
    }
}

const SAVED_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SavedTable {
    counts: Vec<(Vec<u32>, u64)>,
    offered: u64,
    decrements: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedPane {
    index: i64,
    tables: Vec<SavedTable>,
    events: u64,
    folded: u64,
    late: u64,
    forced: u64,
    evicted: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    format: u32,
    params: WatchParams,
    names: Vec<String>,
    tails: Vec<(u64, Vec<u32>, u64)>,
    buffer: Vec<(i64, u64, u64, u32)>,
    watermark: Option<i64>,
    latest: Option<i64>,
    panes: Vec<SavedPane>,
    pending: (u64, u64, u64),
    previous: Option<Previous>,
    totals: Totals,
}
