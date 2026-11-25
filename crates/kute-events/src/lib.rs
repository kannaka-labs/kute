//! Complete event records, kept exactly and stored by column.
//!
//! A token sequence of event names keeps the names and drops everything
//! else an event carried: which entity it belongs to, when it happened,
//! what values came with it. [`pack`] keeps all of it. It reads JSON Lines,
//! one object per event, and writes one container from which [`unpack`]
//! gives back every record, in the order of the source, in canonical JSON
//! (sorted keys, no spaces): the form the `jsonl` adapter writes. When the
//! source was already in that form the bytes are the same, and the
//! container says so.
//!
//! Inside, each kind of thing is stored with its like:
//!
//! - the event names, as indexes into a table, grouped by entity so that
//!   the motifs of one entity sit together;
//! - which entity each event belongs to, in source order;
//! - the times, as differences (from the previous event in the source, or
//!   from the previous event of the same entity, whichever is smaller);
//! - every other field in a column of its own.
//!
//! Each column is then compressed with zlib. Nothing here is learned from
//! other data and no vocabulary is needed to read a container.
//!
//! An entity is identified by the JSON value of the group field, so the
//! number 1 and the string "1" are two entities.

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use kute_types::{sha256_tagged, KuteError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};

/// First bytes of a container.
pub const MAGIC: &[u8; 9] = b"KUTE-EV1\n";
/// Longest line read as one event.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// Most columns a container may declare.
pub const MAX_COLUMNS: usize = 4096;
/// Most bytes of header.
const MAX_HEADER_BYTES: usize = 4 << 20;

/// Which fields of a record are the event name, its time and its entity.
/// Every other field is an attribute and is kept as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// The field holding the event name. Required in every record; a string.
    pub symbol_field: String,
    /// The field holding the time, a whole number. Required when named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
    /// The field naming the entity. Required when named; any JSON scalar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_field: Option<String>,
}

/// How one column is written before compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Canonical JSON texts, one per line.
    Json,
    /// Unsigned integers as LEB128 varints.
    Varint,
    /// Signed whole numbers as zigzag varints of the difference from the
    /// previous value.
    Delta,
}

/// How a column is compressed. Named in every column so that a later
/// codec can sit beside this one in one container; a reader refuses a
/// codec it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Codec {
    /// One zlib stream.
    Zlib,
    /// One Zstandard frame, with a window of at most 128 MiB. Written
    /// and read only by a build with the `zstd` feature.
    Zstd,
}

#[cfg(not(feature = "zstd"))]
const NO_ZSTD: &str = "this build has no Zstandard: build with the `zstd` feature, or use zlib";

impl Codec {
    /// What [`pack`] writes: Zstandard when this build has it, zlib
    /// when it does not.
    pub const fn preferred() -> Self {
        if cfg!(feature = "zstd") {
            Self::Zstd
        } else {
            Self::Zlib
        }
    }

    /// Whether this build reads and writes the codec.
    pub const fn available(self) -> bool {
        matches!(self, Self::Zlib) || cfg!(feature = "zstd")
    }

    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "zlib" => Ok(Self::Zlib),
            "zstd" => Ok(Self::Zstd),
            other => Err(KuteError::InvalidConfig(format!("unknown codec {other:?}; use zstd or zlib"))),
        }
    }
}

#[cfg_attr(not(feature = "zstd"), allow(dead_code))]
/// The largest Zstandard window written, and the largest accepted: 2^27.
const ZSTD_WINDOW_LOG: u32 = 27;

/// One column as the header describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// `@names`, `@entities`, `@shapes`, `@shape`, `@entity`, `@symbols`,
    /// `@time`, or `f:` followed by a field name.
    pub name: String,
    pub kind: Kind,
    /// How the column's bytes are compressed.
    pub codec: Codec,
    /// Bytes before compression.
    pub raw: u64,
    /// Bytes in the container.
    pub stored: u64,
}

/// How time differences were taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimeOrder {
    /// From the previous event in the source.
    Source,
    /// From the previous event of the same entity.
    Entity,
}

/// The header of a container: everything needed to read it and to know
/// what reading it will cost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub layout: Layout,
    pub events: u64,
    /// Bytes [`unpack`] gives back.
    pub canonical_bytes: u64,
    /// SHA-256 of those bytes.
    pub sha256: String,
    /// Whether the source was those bytes exactly.
    pub byte_exact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_order: Option<TimeOrder>,
    pub columns: Vec<Column>,
}

fn invalid(why: impl Into<String>) -> KuteError {
    KuteError::InvalidInput(why.into())
}

fn damaged(why: impl Into<String>) -> KuteError {
    KuteError::Integrity(format!("event container: {}", why.into()))
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn unzigzag(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

/// Reads varints from a column, refusing anything malformed.
struct Varints<'a> {
    bytes: &'a [u8],
    at: usize,
    column: &'a str,
}

impl Varints<'_> {
    fn next(&mut self) -> Result<u64> {
        let mut value: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.at).ok_or_else(|| damaged(format!("column {} ends early", self.column)))?;
            self.at += 1;
            if shift == 63 && byte > 1 {
                break;
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(damaged(format!("column {} holds a number wider than 64 bits", self.column)))
    }

    fn done(&self) -> Result<()> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(damaged(format!("column {} has bytes left over", self.column)))
        }
    }
}

fn deflate(raw: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

#[cfg_attr(not(feature = "zstd"), allow(dead_code))]
/// The zstd window for a column of `raw` bytes: enough to cover it,
/// within what a reader accepts.
fn window_log(raw: u64) -> u32 {
    (u64::BITS - raw.saturating_sub(1).leading_zeros()).clamp(10, ZSTD_WINDOW_LOG)
}

fn compress(raw: &[u8], codec: Codec) -> Result<Vec<u8>> {
    match codec {
        Codec::Zlib => deflate(raw),
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => Err(KuteError::InvalidConfig(NO_ZSTD.into())),
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            // A window that covers the column, up to the largest a reader
            // accepts, with long-distance matching once the window is
            // larger than the level would use by itself.
            let window = window_log(raw.len() as u64);
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
            encoder.window_log(window)?;
            encoder.long_distance_matching(window > 23)?;
            encoder.include_checksum(false)?;
            encoder.set_pledged_src_size(Some(raw.len() as u64))?;
            encoder.write_all(raw)?;
            Ok(encoder.finish()?)
        }
    }
}

/// About how small a column will be, for choosing between two ways of
/// writing it. Quicker than [`compress`] and close enough to rank them.
fn rough(raw: &[u8], codec: Codec) -> Result<usize> {
    Ok(match codec {
        Codec::Zlib => deflate(raw)?.len(),
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => return Err(KuteError::InvalidConfig(NO_ZSTD.into())),
        #[cfg(feature = "zstd")]
        Codec::Zstd => zstd::bulk::compress(raw, 6)?.len(),
    })
}

/// Inflate one column to exactly its declared size, refusing a column
/// that is longer, shorter, or followed by bytes it does not use.
fn expand(stored: &[u8], column: &Column) -> Result<Vec<u8>> {
    let bad = |why: String| damaged(format!("column {}: {why}", column.name));
    let mut bytes = Vec::new();
    let limit = column.raw.saturating_add(1);
    let spare = match column.codec {
        Codec::Zlib => {
            let mut decoder = ZlibDecoder::new(stored);
            decoder.by_ref().take(limit).read_to_end(&mut bytes).map_err(|e| bad(e.to_string()))?;
            stored.len() as u64 - decoder.total_in()
        }
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => return Err(KuteError::InvalidConfig(format!("column {} is Zstandard; {NO_ZSTD}", column.name))),
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(stored).map_err(|e| bad(e.to_string()))?.single_frame();
            // No more window than the writer would have asked for a
            // column of this size: a frame that declares a large window
            // around a few bytes is refused before the memory is taken.
            decoder.window_log_max(window_log(column.raw)).map_err(|e| bad(e.to_string()))?;
            decoder.by_ref().take(limit).read_to_end(&mut bytes).map_err(|e| bad(e.to_string()))?;
            decoder.finish().len() as u64
        }
    };
    if bytes.len() as u64 != column.raw {
        return Err(damaged(format!("column {} does not inflate to its declared size", column.name)));
    }
    if spare != 0 {
        return Err(damaged(format!("column {} has bytes after its compressed data", column.name)));
    }
    Ok(bytes)
}

/// A table of canonical JSON texts, numbered in order of first use.
#[derive(Default)]
struct Table {
    index: HashMap<Vec<u8>, u64>,
    texts: Vec<Vec<u8>>,
}

impl Table {
    fn id(&mut self, text: &[u8]) -> u64 {
        if let Some(&id) = self.index.get(text) {
            return id;
        }
        let id = self.texts.len() as u64;
        self.index.insert(text.to_vec(), id);
        self.texts.push(text.to_vec());
        id
    }

    fn lines(&self) -> Vec<u8> {
        self.texts.join(&b'\n')
    }
}

/// The values of one attribute field, in source order.
#[derive(Default)]
struct Field {
    /// Canonical JSON text of each value.
    texts: Vec<Vec<u8>>,
    /// The values as whole numbers, while all of them have been.
    whole: Option<Vec<i64>>,
}

fn whole_number(value: &Value, text: &[u8]) -> Option<i64> {
    // Only a number written as a plain integer: `5.0` and `5e0` are kept
    // as text, so that they come back as they were.
    let n = value.as_i64()?;
    (n.to_string().as_bytes() == text).then_some(n)
}

fn delta_column(values: impl Iterator<Item = i64>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last: i64 = 0;
    for v in values {
        put_varint(&mut out, zigzag(v.wrapping_sub(last)));
        last = v;
    }
    out
}

/// What [`pack`] did, for a report.
#[derive(Clone, Debug, Serialize)]
pub struct Packed {
    pub header: Header,
    /// Bytes of the source.
    pub source_bytes: u64,
    /// Bytes of the container, header included.
    pub container_bytes: u64,
    pub entities: u64,
    pub event_names: u64,
    pub shapes: u64,
}

/// Pack JSON Lines into a container. Blank lines are skipped; anything
/// else that is not a JSON object with the fields the layout requires is
/// an error naming the line. Nothing is dropped or rounded.
pub fn pack(source: &[u8], layout: &Layout) -> Result<(Vec<u8>, Packed)> {
    pack_with(source, layout, Codec::preferred())
}

/// [`pack`] with the codec chosen. `zlib` writes a container that needs
/// nothing but a zlib reader; `zstd` writes a smaller one and needs a
/// build with the `zstd` feature, here and in the reader.
pub fn pack_with(source: &[u8], layout: &Layout, codec: Codec) -> Result<(Vec<u8>, Packed)> {
    if layout.symbol_field.is_empty() {
        return Err(KuteError::InvalidConfig("the symbol field must be named".into()));
    }
    let special = |key: &str| key == layout.symbol_field || layout.time_field.as_deref() == Some(key) || layout.group_field.as_deref() == Some(key);
    if layout.time_field.as_deref() == Some(layout.symbol_field.as_str())
        || layout.group_field.as_deref() == Some(layout.symbol_field.as_str())
        || (layout.time_field.is_some() && layout.time_field == layout.group_field)
    {
        return Err(KuteError::InvalidConfig("the symbol, time and group fields must be three different fields".into()));
    }

    let (mut names, mut entities, mut shapes) = (Table::default(), Table::default(), Table::default());
    let (mut shape_of, mut entity_of, mut symbol_of, mut times): (Vec<u64>, Vec<u64>, Vec<u64>, Vec<i64>) = Default::default();
    let mut fields: Vec<(String, Field)> = Vec::new();
    let mut field_index: HashMap<String, usize> = HashMap::new();
    let mut hasher_input_len: u64 = 0;
    let mut canonical_all = sha2::Sha256::default();
    let mut byte_exact = true;

    for (n, line) in source.split(|&b| b == b'\n').enumerate() {
        let n = n + 1;
        let at = |why: String| invalid(format!("line {n}: {why}"));
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            if !line.is_empty() {
                byte_exact = false;
            }
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(at("longer than 1 MiB".into()));
        }
        let (value, canonical) = kute_adapters::canonical_json(line, &format!("line {n}"))?;
        let object = value.as_object().ok_or_else(|| at("not a JSON object".into()))?;
        if canonical != line {
            byte_exact = false;
        }
        sha2::Digest::update(&mut canonical_all, &canonical);
        sha2::Digest::update(&mut canonical_all, b"\n");
        hasher_input_len += canonical.len() as u64 + 1;

        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        let shape = serde_json::to_vec(&keys)?;
        shape_of.push(shapes.id(&shape));

        match object.get(&layout.symbol_field) {
            Some(Value::String(s)) if !s.is_empty() => symbol_of.push(names.id(&serde_json::to_vec(s)?)),
            Some(Value::String(_)) => return Err(at(format!("field {:?} is empty", layout.symbol_field))),
            Some(_) => return Err(at(format!("field {:?} is not a string", layout.symbol_field))),
            None => return Err(at(format!("no field {:?}", layout.symbol_field))),
        }
        if let Some(field) = &layout.group_field {
            match object.get(field) {
                Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => entity_of.push(entities.id(&serde_json::to_vec(v)?)),
                Some(_) => return Err(at(format!("field {field:?} is not a string, a number or a boolean"))),
                None => return Err(at(format!("no field {field:?}"))),
            }
        }
        if let Some(field) = &layout.time_field {
            match object.get(field) {
                Some(v) => {
                    let text = serde_json::to_vec(v)?;
                    times.push(whole_number(v, &text).ok_or_else(|| at(format!("field {field:?} is not a whole number written as an integer; choose a finer time unit")))?);
                }
                None => return Err(at(format!("no field {field:?}"))),
            }
        }
        for key in keys {
            if special(key) {
                continue;
            }
            let slot = match field_index.get(key.as_str()) {
                Some(&i) => i,
                None => {
                    if fields.len() + 8 >= MAX_COLUMNS {
                        return Err(at(format!("more than {} different field names", MAX_COLUMNS - 8)));
                    }
                    fields.push((key.clone(), Field { texts: Vec::new(), whole: Some(Vec::new()) }));
                    field_index.insert(key.clone(), fields.len() - 1);
                    fields.len() - 1
                }
            };
            let field = &mut fields[slot].1;
            let text = serde_json::to_vec(&sorted(&object[key]))?;
            match (whole_number(&object[key], &text), &mut field.whole) {
                (Some(n), Some(whole)) => whole.push(n),
                _ => field.whole = None,
            }
            field.texts.push(text);
        }
    }
    if source.last().is_some_and(|&b| b != b'\n') {
        byte_exact = false;
    }
    let events = symbol_of.len() as u64;
    let sha256 = format!("sha256:{}", hex(&sha2::Digest::finalize(canonical_all)));

    // Symbols grouped by entity, entities in order of first appearance,
    // each entity's events in source order.
    let grouped_symbols: Vec<u64> = if layout.group_field.is_some() {
        let mut order: Vec<usize> = (0..symbol_of.len()).collect();
        order.sort_by_key(|&i| entity_of[i]);
        order.into_iter().map(|i| symbol_of[i]).collect()
    } else {
        symbol_of
    };

    let mut columns: Vec<(Column, Vec<u8>)> = Vec::new();
    let mut add = |name: String, kind: Kind, raw: Vec<u8>| -> Result<()> {
        let stored = compress(&raw, codec)?;
        columns.push((Column { name, kind, codec, raw: raw.len() as u64, stored: stored.len() as u64 }, stored));
        Ok(())
    };
    let varints = |values: &[u64]| {
        let mut out = Vec::with_capacity(values.len());
        for &v in values {
            put_varint(&mut out, v);
        }
        out
    };
    add("@names".into(), Kind::Json, names.lines())?;
    add("@shapes".into(), Kind::Json, shapes.lines())?;
    add("@shape".into(), Kind::Varint, varints(&shape_of))?;
    if layout.group_field.is_some() {
        add("@entities".into(), Kind::Json, entities.lines())?;
        add("@entity".into(), Kind::Varint, varints(&entity_of))?;
    }
    add("@symbols".into(), Kind::Varint, varints(&grouped_symbols))?;
    let mut time_order = None;
    if layout.time_field.is_some() {
        let by_source = delta_column(times.iter().copied());
        let mut choice = (TimeOrder::Source, by_source);
        if layout.group_field.is_some() {
            let mut last: Vec<i64> = vec![0; entities.texts.len()];
            let mut by_entity = Vec::new();
            for (i, &t) in times.iter().enumerate() {
                let e = entity_of[i] as usize;
                put_varint(&mut by_entity, zigzag(t.wrapping_sub(last[e])));
                last[e] = t;
            }
            if rough(&by_entity, codec)? < rough(&choice.1, codec)? {
                choice = (TimeOrder::Entity, by_entity);
            }
        }
        time_order = Some(choice.0);
        add("@time".into(), Kind::Delta, choice.1)?;
    }
    for (name, field) in fields {
        let text = field.texts.join(&b'\n');
        match field.whole {
            Some(whole) => {
                let delta = delta_column(whole.into_iter());
                if rough(&delta, codec)? < rough(&text, codec)? {
                    add(format!("f:{name}"), Kind::Delta, delta)?;
                } else {
                    add(format!("f:{name}"), Kind::Json, text)?;
                }
            }
            None => add(format!("f:{name}"), Kind::Json, text)?,
        }
    }

    let header = Header {
        format: 1,
        layout: layout.clone(),
        events,
        canonical_bytes: hasher_input_len,
        sha256,
        byte_exact,
        time_order,
        columns: columns.iter().map(|(c, _)| c.clone()).collect(),
    };
    let header_bytes = serde_json::to_vec(&header)?;
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + header_bytes.len() + columns.iter().map(|(_, b)| b.len()).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    for (_, bytes) in &columns {
        out.extend_from_slice(bytes);
    }
    let report = Packed {
        header,
        source_bytes: source.len() as u64,
        container_bytes: out.len() as u64,
        entities: entities.texts.len() as u64,
        event_names: names.texts.len() as u64,
        shapes: shapes.texts.len() as u64,
    };
    Ok((out, report))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A value with the keys of every object inside it sorted.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&map[k]))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// The field names of one shape, refused past [`MAX_COLUMNS`] while they
/// are being read and not after.
struct ShapeKeys(Vec<String>);

impl<'de> Deserialize<'de> for ShapeKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Keys;
        impl<'de> serde::de::Visitor<'de> for Keys {
            type Value = ShapeKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a list of at most {MAX_COLUMNS} field names")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<ShapeKeys, A::Error> {
                let mut names = Vec::new();
                while let Some(name) = seq.next_element::<String>()? {
                    if names.len() >= MAX_COLUMNS {
                        return Err(serde::de::Error::custom("more field names than a container may have"));
                    }
                    names.push(name);
                }
                Ok(ShapeKeys(names))
            }
        }
        deserializer.deserialize_seq(Keys)
    }
}

/// Read the header of a container without reading its columns.
pub fn header(container: &[u8]) -> Result<(Header, usize)> {
    if container.len() < MAGIC.len() + 4 || &container[..MAGIC.len()] != MAGIC {
        return Err(damaged("not an event container"));
    }
    let length = u32::from_le_bytes(container[MAGIC.len()..MAGIC.len() + 4].try_into().expect("four bytes")) as usize;
    let start = MAGIC.len() + 4;
    if length > MAX_HEADER_BYTES || length > container.len() - start {
        return Err(damaged("the header is longer than the container or than 4 MiB"));
    }
    let header: Header = serde_json::from_slice(&container[start..start + length]).map_err(|e| damaged(format!("header: {e}")))?;
    if header.format != 1 {
        return Err(KuteError::Unsupported(format!("event container format {}", header.format)));
    }
    if header.columns.len() > MAX_COLUMNS {
        return Err(damaged("too many columns"));
    }
    Ok((header, start + length))
}

/// Unpack a container into canonical JSON Lines, refusing before any
/// column is inflated if the result would be longer than `max_bytes`.
///
/// Memory while unpacking: the result, at most `max_bytes`; the columns,
/// refused together past twice `max_bytes` plus 1 MiB; and what is kept
/// for each event to find its name, entity and shape, which is some tens
/// of bytes an event. A header may declare one event for every ten bytes
/// of result, so the worst case is several times `max_bytes`; for records
/// of a hundred bytes or more it is less than `max_bytes`. Tables are
/// refused when they have more entries than there are events, and shapes
/// when they name more fields than the result could hold. The result is
/// checked against the length and the SHA-256 in the header.
pub fn unpack(container: &[u8], max_bytes: u64) -> Result<Vec<u8>> {
    let (header, mut at) = header(container)?;
    if header.canonical_bytes > max_bytes {
        return Err(KuteError::LimitExceeded(format!("the container unpacks to {} bytes; the limit is {max_bytes}", header.canonical_bytes)));
    }
    // A record is at least `{"k":"v"}` and a line break.
    if header.events > header.canonical_bytes / 10 + 1 {
        return Err(damaged("more events than the declared bytes can hold"));
    }
    let budget = max_bytes.saturating_mul(2).saturating_add(1 << 20);
    let declared = header.columns.iter().try_fold(0u64, |sum, c| sum.checked_add(c.raw)).ok_or_else(|| damaged("column sizes overflow"))?;
    if declared > budget {
        return Err(KuteError::LimitExceeded(format!("the columns inflate to {declared} bytes; the limit is {budget}")));
    }
    let mut raw: HashMap<&str, (Kind, Vec<u8>)> = HashMap::new();
    for column in &header.columns {
        let stored = usize::try_from(column.stored).ok().filter(|&n| n <= container.len() - at).ok_or_else(|| damaged(format!("column {} runs past the end", column.name)))?;
        let bytes = expand(&container[at..at + stored], column)?;
        at += stored;
        if raw.insert(column.name.as_str(), (column.kind, bytes)).is_some() {
            return Err(damaged(format!("column {} appears twice", column.name)));
        }
    }
    if at != container.len() {
        return Err(damaged("bytes after the last column"));
    }
    let take = |name: &str, kind: Kind| -> Result<&[u8]> {
        match raw.get(name) {
            Some((k, bytes)) if *k == kind => Ok(bytes.as_slice()),
            Some(_) => Err(damaged(format!("column {name} is of the wrong kind"))),
            None => Err(damaged(format!("no column {name}"))),
        }
    };
    let events = usize::try_from(header.events).map_err(|_| damaged("too many events"))?;
    // A table has an entry only for something an event used, so it cannot
    // have more entries than there are events. Counted before the entries
    // are collected: a column of nothing but line breaks would otherwise
    // become sixteen bytes of table for each byte of column.
    let table = |name: &str| -> Result<Vec<&[u8]>> {
        let bytes = take(name, Kind::Json)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        if bytes.iter().filter(|&&b| b == b'\n').count() >= events {
            return Err(damaged(format!("column {name} has more entries than there are events")));
        }
        Ok(bytes.split(|&b| b == b'\n').collect())
    };
    let numbers = |name: &'static str, limit: usize| -> Result<Vec<usize>> {
        let mut reader = Varints { bytes: take(name, Kind::Varint)?, at: 0, column: name };
        let mut out = Vec::with_capacity(events.min(1 << 20));
        for _ in 0..events {
            let v = reader.next()?;
            out.push(usize::try_from(v).ok().filter(|&v| v < limit).ok_or_else(|| damaged(format!("column {name} points outside its table")))?);
        }
        reader.done()?;
        Ok(out)
    };

    let names = table("@names")?;
    let shape_texts = table("@shapes")?;
    // Field names are kept once and shapes refer to them, so what the
    // shapes cost is counted in field uses, and those are bounded by the
    // bytes the records must take: a field is at least `"":0` and a comma.
    let mut keys: Vec<(Vec<u8>, String)> = Vec::new();
    let mut key_index: HashMap<String, u32> = HashMap::new();
    let mut shapes: Vec<Vec<u32>> = Vec::with_capacity(shape_texts.len());
    let mut uses_left = header.canonical_bytes / 5 + 1;
    for text in shape_texts {
        let ShapeKeys(names) = serde_json::from_slice(text).map_err(|e| damaged(format!("a shape is not a list of field names: {e}")))?;
        // Sorted and without repeats, as `pack` writes them: a field named
        // twice would be read twice from its column.
        if names.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(damaged("a shape's field names are not sorted, or one repeats"));
        }
        uses_left = uses_left.checked_sub(names.len() as u64).ok_or_else(|| damaged("the shapes name more fields than the declared bytes can hold"))?;
        let mut shape = Vec::with_capacity(names.len());
        for name in names {
            let id = match key_index.get(&name) {
                Some(&id) => id,
                None => {
                    if keys.len() >= MAX_COLUMNS {
                        return Err(damaged("too many different field names"));
                    }
                    let id = keys.len() as u32;
                    keys.push((serde_json::to_vec(&name)?, name.clone()));
                    key_index.insert(name, id);
                    id
                }
            };
            shape.push(id);
        }
        shapes.push(shape);
    }
    let shape_of = numbers("@shape", shapes.len())?;
    let layout = &header.layout;
    let (entities, entity_of) = if layout.group_field.is_some() {
        let entities = table("@entities")?;
        let entity_of = numbers("@entity", entities.len())?;
        (entities, entity_of)
    } else {
        (Vec::new(), Vec::new())
    };
    let grouped_symbols = numbers("@symbols", names.len())?;
    // Where each entity's symbols start in the grouped column.
    let mut next_symbol: Vec<usize> = Vec::new();
    if layout.group_field.is_some() {
        let mut counts = vec![0usize; entities.len()];
        for &e in &entity_of {
            counts[e] += 1;
        }
        let mut start = 0;
        for count in counts {
            next_symbol.push(start);
            start += count;
        }
    }
    let mut time = match &layout.time_field {
        Some(_) => Some(Varints { bytes: take("@time", Kind::Delta)?, at: 0, column: "@time" }),
        None => None,
    };
    let time_order = header.time_order.unwrap_or(TimeOrder::Source);
    if time_order == TimeOrder::Entity && layout.group_field.is_none() {
        return Err(damaged("times by entity without entities"));
    }
    let mut last_time: Vec<i64> = vec![0; entities.len().max(1)];

    enum Reader<'a> {
        Json(std::slice::Split<'a, u8, fn(&u8) -> bool>),
        Delta(Varints<'a>, i64),
    }
    fn is_newline(b: &u8) -> bool {
        *b == b'\n'
    }
    let mut readers: HashMap<&str, Reader> = HashMap::new();
    for column in &header.columns {
        if let Some(field) = column.name.strip_prefix("f:") {
            let bytes = raw[column.name.as_str()].1.as_slice();
            let reader = match column.kind {
                Kind::Json => Reader::Json(bytes.split(is_newline as fn(&u8) -> bool)),
                Kind::Delta => Reader::Delta(Varints { bytes, at: 0, column: &column.name }, 0),
                Kind::Varint => return Err(damaged(format!("column {} is of the wrong kind", column.name))),
            };
            readers.insert(field, reader);
        }
    }

    let capacity = usize::try_from(header.canonical_bytes).map_err(|_| damaged("too many bytes"))?;
    // A header can declare more than its columns hold; grow as records arrive.
    let mut out: Vec<u8> = Vec::with_capacity(capacity.min(64 << 20));
    let over = || damaged("the records are longer than the header declares");
    for i in 0..events {
        let entity = if layout.group_field.is_some() { entity_of[i] } else { 0 };
        out.push(b'{');
        for (k, &key_id) in shapes[shape_of[i]].iter().enumerate() {
            let (key_text, key) = &keys[key_id as usize];
            if k > 0 {
                out.push(b',');
            }
            out.extend_from_slice(key_text);
            out.push(b':');
            if *key == layout.symbol_field {
                let position = if layout.group_field.is_some() {
                    let p = next_symbol[entity];
                    next_symbol[entity] += 1;
                    p
                } else {
                    i
                };
                out.extend_from_slice(names[grouped_symbols[position]]);
            } else if layout.group_field.as_deref() == Some(key.as_str()) {
                out.extend_from_slice(entities[entity]);
            } else if layout.time_field.as_deref() == Some(key.as_str()) {
                let reader = time.as_mut().expect("a time field has a time column");
                let slot = if time_order == TimeOrder::Entity { entity } else { 0 };
                let t = last_time[slot].wrapping_add(unzigzag(reader.next()?));
                last_time[slot] = t;
                out.extend_from_slice(t.to_string().as_bytes());
            } else {
                match readers.get_mut(key.as_str()) {
                    Some(Reader::Json(values)) => out.extend_from_slice(values.next().ok_or_else(|| damaged(format!("column f:{key} ends early")))?),
                    Some(Reader::Delta(values, last)) => {
                        *last = last.wrapping_add(unzigzag(values.next()?));
                        out.extend_from_slice(last.to_string().as_bytes());
                    }
                    None => return Err(damaged(format!("no column for the field {key:?}"))),
                }
            }
            if out.len() > capacity {
                return Err(over());
            }
        }
        out.extend_from_slice(b"}\n");
        if out.len() > capacity {
            return Err(over());
        }
    }
    // Nothing may be left in a column that no record asked for.
    if let Some(reader) = &time {
        reader.done()?;
    }
    for (field, reader) in &mut readers {
        let spent = match reader {
            Reader::Json(values) => values.next().is_none(),
            Reader::Delta(values, _) => values.done().is_ok(),
        };
        if !spent {
            return Err(damaged(format!("column f:{field} holds more values than the records use")));
        }
    }
    if out.len() != capacity || sha256_tagged(&out) != header.sha256 {
        return Err(damaged("the records do not match the length and digest in the header"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(raw: u64, stored: usize) -> Column {
        Column { name: "f:x".into(), kind: Kind::Json, codec: Codec::Zstd, raw, stored: stored as u64 }
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn without_zstandard_a_zstandard_column_is_refused_by_name() {
        assert!(!Codec::Zstd.available() && Codec::preferred() == Codec::Zlib);
        assert!(compress(b"abc", Codec::Zstd).unwrap_err().to_string().contains("zstd"));
        assert!(expand(b"abc", &column(3, 3)).unwrap_err().to_string().contains("zstd"));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn a_frame_asking_for_more_window_than_its_column_needs_is_refused() {
        // Three bytes, in a frame whose header asks for a 128 MiB window.
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        encoder.window_log(ZSTD_WINDOW_LOG).unwrap();
        encoder.write_all(b"abc").unwrap();
        let greedy = encoder.finish().unwrap();
        assert!(expand(&greedy, &column(3, greedy.len())).is_err());
        // What the writer makes for the same bytes is read back.
        let honest = compress(b"abc", Codec::Zstd).unwrap();
        assert_eq!(expand(&honest, &column(3, honest.len())).unwrap(), b"abc");
        // And a large column still gets the window it needs.
        let big: Vec<u8> = (0..3_000_000u32).flat_map(|i| (i.wrapping_mul(2_654_435_761) >> 7).to_le_bytes()).collect();
        let stored = compress(&big, Codec::Zstd).unwrap();
        assert_eq!(expand(&stored, &column(big.len() as u64, stored.len())).unwrap(), big);
    }

    #[test]
    fn the_window_covers_the_column_within_what_a_reader_accepts() {
        assert_eq!(window_log(0), 10);
        assert_eq!(window_log(1024), 10);
        assert_eq!(window_log(1025), 11);
        assert_eq!(window_log(1 << 27), 27);
        assert_eq!(window_log(u64::MAX), 27);
    }
}
