//! Typed event streams: symbols that keep their time and their grouping.
//!
//! A [`crate::Payload`] of symbols is an ordered list of names and nothing
//! else. An [`EventStream`] is the same list with what the source knew about
//! it kept alongside: which entity each event belongs to, when it happened,
//! and the schema that says how both were read. Tokenizers and discovery
//! still consume the symbols; the times stay aligned with them, position by
//! position, for whatever needs them afterwards.

use crate::{sha256_tagged, KuteError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;

/// Most events one stream will hold.
pub const MAX_EVENTS: u64 = 1 << 32;
/// Longest line the JSONL reader accepts.
pub const MAX_EVENT_LINE: usize = 1 << 20;

/// What one tick of a timestamp means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl TimeUnit {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "s" | "seconds" => TimeUnit::Seconds,
            "ms" | "milliseconds" => TimeUnit::Milliseconds,
            "us" | "microseconds" => TimeUnit::Microseconds,
            "ns" | "nanoseconds" => TimeUnit::Nanoseconds,
            other => return Err(KuteError::InvalidConfig(format!("time unit {other:?}: expected s, ms, us or ns"))),
        })
    }

    pub fn ticks_per_second(self) -> f64 {
        match self {
            TimeUnit::Seconds => 1.0,
            TimeUnit::Milliseconds => 1e3,
            TimeUnit::Microseconds => 1e6,
            TimeUnit::Nanoseconds => 1e9,
        }
    }
}

/// How events were read from their source. The schema travels with the
/// stream and is part of its fingerprint, so two streams read from the same
/// file under different schemas are different streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSchema {
    /// A name for this schema, chosen by whoever defines it.
    pub id: String,
    /// Bumped when the meaning of a field changes.
    pub version: u32,
    /// Field holding the event name.
    pub symbol_field: String,
    /// Field holding the timestamp: an integer count of `time_unit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_unit: Option<TimeUnit>,
    /// Field naming the entity an event belongs to. Events with the same
    /// value form one record. Without it, the whole source is one record.
    /// A numeric value is read as its text: `1` and `"1"` name the same
    /// record, and `1.0` names another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_field: Option<String>,
}

impl EventSchema {
    pub fn validate(&self) -> Result<()> {
        let bad = |why: &str| Err(KuteError::InvalidConfig(format!("event schema: {why}")));
        if self.id.is_empty() || self.id.len() > 256 {
            return bad("id must be 1 to 256 bytes");
        }
        for field in [Some(&self.symbol_field), self.time_field.as_ref(), self.group_field.as_ref()].into_iter().flatten() {
            if field.is_empty() || field.len() > 256 {
                return bad("field names must be 1 to 256 bytes");
            }
        }
        if self.time_field.is_some() != self.time_unit.is_some() {
            return bad("a time field and a time unit come together or not at all");
        }
        Ok(())
    }
}

/// The events of one entity, in time order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Value of the group field, when the schema has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub symbols: Vec<String>,
    /// One timestamp per symbol, or empty when the schema has no time field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub times: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStream {
    pub schema: EventSchema,
    pub records: Vec<EventRecord>,
}

impl EventStream {
    pub fn new(schema: EventSchema) -> Result<Self> {
        schema.validate()?;
        Ok(EventStream { schema, records: Vec::new() })
    }

    pub fn events(&self) -> u64 {
        self.records.iter().map(|r| r.symbols.len() as u64).sum()
    }

    pub fn timed(&self) -> bool {
        self.schema.time_field.is_some()
    }

    /// Structural check: times aligned with symbols and never going back.
    pub fn validate(&self) -> Result<()> {
        self.schema.validate()?;
        for (i, r) in self.records.iter().enumerate() {
            let bad = |why: &str| Err(KuteError::InvalidInput(format!("event record {i}: {why}")));
            if r.symbols.is_empty() {
                return bad("no events");
            }
            if r.symbols.iter().any(|s| s.is_empty() || s.len() > crate::vocab::MAX_SYMBOL_BYTES) {
                return bad("an event name is empty or oversized");
            }
            if self.timed() {
                if r.times.len() != r.symbols.len() {
                    return bad("timestamps are not aligned with events");
                }
                if r.times.windows(2).any(|w| w[1] < w[0]) {
                    return bad("timestamps go backwards");
                }
            } else if !r.times.is_empty() {
                return bad("timestamps present but the schema has no time field");
            }
        }
        Ok(())
    }

    /// The symbols alone, one sequence per record.
    pub fn symbol_records(&self) -> Vec<Vec<String>> {
        self.records.iter().map(|r| r.symbols.clone()).collect()
    }

    /// SHA-256 over the schema and every record: keys, names and times.
    pub fn fingerprint(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        let schema = serde_json::to_vec(&self.schema)?;
        h.update((schema.len() as u64).to_le_bytes());
        h.update(&schema);
        for r in &self.records {
            let key = r.key.as_deref().unwrap_or("");
            h.update([r.key.is_some() as u8]);
            h.update((key.len() as u64).to_le_bytes());
            h.update(key);
            h.update((r.symbols.len() as u64).to_le_bytes());
            for s in &r.symbols {
                h.update((s.len() as u64).to_le_bytes());
                h.update(s);
            }
            h.update((r.times.len() as u64).to_le_bytes());
            for t in &r.times {
                h.update(t.to_le_bytes());
            }
        }
        Ok(format!("sha256:{}", crate::hex(&h.finalize())))
    }

    /// Split into the first records and the last `fraction` of them. Each
    /// side gets at least one record; a stream with fewer than two is an error.
    pub fn split_last(mut self, fraction: f64) -> Result<(EventStream, EventStream)> {
        if !(fraction > 0.0 && fraction < 1.0) {
            return Err(KuteError::InvalidConfig("the held-out fraction must be between 0 and 1".into()));
        }
        if self.records.len() < 2 {
            return Err(KuteError::InvalidInput("splitting needs at least two records, one for each side".into()));
        }
        let cut = ((self.records.len() as f64) * (1.0 - fraction)).round() as usize;
        // Both sides keep at least one record, whatever the fraction rounds to.
        let tail = self.records.split_off(cut.clamp(1, self.records.len() - 1));
        let schema = self.schema.clone();
        Ok((self, EventStream { schema, records: tail }))
    }

    /// Read JSON Lines: one object per line, one event per object.
    ///
    /// Records are formed by the group field and listed in order of first
    /// appearance. Within a record events are put in time order; events with
    /// equal times keep the order of the file. A line without the symbol
    /// field, with a symbol that is not a string, or with a time that is not
    /// a whole number is an error: nothing is skipped or rounded.
    pub fn read_jsonl(&mut self, reader: impl BufRead, source: &str) -> Result<()> {
        let schema = self.schema.clone();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (i, r) in self.records.iter().enumerate() {
            if let Some(k) = &r.key {
                index.insert(k.clone(), i);
            }
        }
        let mut total = self.events();
        let mut touched = vec![false; self.records.len()];
        let mut reader = reader;
        let mut line: Vec<u8> = Vec::new();
        let mut n = 0usize;
        loop {
            // At most one byte past the limit is ever read for one line, so
            // a source with no line breaks cannot be pulled into memory.
            line.clear();
            let read = std::io::Read::take(&mut reader, MAX_EVENT_LINE as u64 + 1).read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            n += 1;
            let at = |why: String| KuteError::InvalidInput(format!("{source}, line {n}: {why}"));
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.len() > MAX_EVENT_LINE {
                return Err(at("longer than 1 MiB".into()));
            }
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&line).map_err(|e| at(format!("not JSON: {e}")))?;
            let object = value.as_object().ok_or_else(|| at("not a JSON object".into()))?;
            let symbol = match object.get(&schema.symbol_field) {
                Some(serde_json::Value::String(s)) if !s.is_empty() && s.len() <= crate::vocab::MAX_SYMBOL_BYTES => s.clone(),
                Some(serde_json::Value::String(_)) => return Err(at(format!("field {:?} is empty or oversized", schema.symbol_field))),
                Some(_) => return Err(at(format!("field {:?} is not a string", schema.symbol_field))),
                None => return Err(at(format!("no field {:?}", schema.symbol_field))),
            };
            let time = match &schema.time_field {
                None => None,
                Some(field) => Some(match object.get(field) {
                    Some(serde_json::Value::Number(x)) => match x.as_i64() {
                        Some(t) => t,
                        None => match x.as_f64() {
                            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => f as i64,
                            _ => return Err(at(format!("field {field:?} is not a whole number; choose a finer time unit"))),
                        },
                    },
                    Some(_) => return Err(at(format!("field {field:?} is not a number"))),
                    None => return Err(at(format!("no field {field:?}"))),
                }),
            };
            let slot = match &schema.group_field {
                None => {
                    if self.records.is_empty() {
                        self.records.push(EventRecord { key: None, symbols: Vec::new(), times: Vec::new() });
                        touched.push(false);
                    }
                    0
                }
                Some(field) => {
                    let key = match object.get(field) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Number(x)) => x.to_string(),
                        Some(_) => return Err(at(format!("field {field:?} is neither a string nor a number"))),
                        None => return Err(at(format!("no field {field:?}"))),
                    };
                    match index.get(&key) {
                        Some(&i) => i,
                        None => {
                            self.records.push(EventRecord { key: Some(key.clone()), symbols: Vec::new(), times: Vec::new() });
                            touched.push(false);
                            index.insert(key, self.records.len() - 1);
                            self.records.len() - 1
                        }
                    }
                }
            };
            total += 1;
            if total > MAX_EVENTS {
                return Err(KuteError::LimitExceeded("more than 2^32 events".into()));
            }
            let record = &mut self.records[slot];
            if let Some(t) = time {
                if record.times.last().is_some_and(|&last| t < last) {
                    touched[slot] = true;
                }
                record.times.push(t);
            }
            record.symbols.push(symbol);
        }
        // Stable sort of the records that arrived out of time order.
        for (record, _) in self.records.iter_mut().zip(&touched).filter(|&(_, &t)| t) {
            let mut order: Vec<usize> = (0..record.times.len()).collect();
            order.sort_by_key(|&i| record.times[i]);
            record.times = order.iter().map(|&i| record.times[i]).collect();
            let mut symbols: Vec<Option<String>> = std::mem::take(&mut record.symbols).into_iter().map(Some).collect();
            record.symbols = order.iter().map(|&i| symbols[i].take().expect("each index once")).collect();
        }
        Ok(())
    }
}

/// Fingerprint helper kept next to the type for callers that only have bytes.
pub fn schema_fingerprint(schema: &EventSchema) -> Result<String> {
    Ok(sha256_tagged(&serde_json::to_vec(schema)?))
}
