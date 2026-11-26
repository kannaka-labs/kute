//! Numeric sequences for KUTE: a signal kept exactly, or within an error
//! bound that is checked on every value before the container is written.
//!
//! A signal is a sequence of `f64`. [`pack`] stores it one of two ways:
//!
//! * **Exact.** Every value comes back with the same bits, including
//!   `NaN`, the infinities and negative zero.
//! * **Bounded.** Every finite value comes back within `max_error` of
//!   itself (absolute), and values that are not finite come back exactly.
//!   The bound is not a property of the method taken on trust: packing
//!   reconstructs each value the way a reader will and keeps exactly any
//!   value that would miss the bound.
//!
//! How: values are turned into whole numbers (a decimal scale found in
//! the data, or steps of twice the error bound), differences are taken
//! (order 0, 1 or 2, whichever stores smallest), written as varints and
//! compressed, with Zstandard (the `zstd` feature, on by default) or
//! with zlib. Values that do not fit the scheme are kept as
//! raw bits beside it. A signal with no decimal scale is stored exactly
//! as its bytes, arranged by byte position.
//!
//! These are standard techniques. What the container adds is the
//! contract: the header states the mode, the bound, the largest error
//! actually incurred and a hash of the values a reader must obtain, and
//! [`unpack`] refuses a container that does not reproduce them.
//!
//! What it is not: a format for text. `"234.840"` and `"234.84"` are the
//! same value and come back as the same value. Nor a model of the
//! signal: nothing is predicted beyond the last one or two values.

use kute_types::{KuteError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub const MAGIC: &[u8; 9] = b"KUTE-SG1\n";
/// Most values in one container.
pub const MAX_VALUES: u64 = 1 << 31;
const MAX_HEADER_BYTES: usize = 64 * 1024;
#[cfg_attr(not(feature = "zstd"), allow(dead_code))]
const ZSTD_WINDOW_LOG: u32 = 27;
/// Whole numbers are kept below this size so that turning them back into
/// `f64` is exact with room to spare.
const MAX_WHOLE: f64 = (1u64 << 50) as f64;
/// Most decimal places looked for.
const MAX_DECIMALS: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Exact,
    /// Every finite value within this absolute error.
    Bounded { max_error: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scheme {
    /// Whole numbers of 10^-decimals.
    Decimal,
    /// Whole numbers of `step`.
    Steps,
    /// The eight bytes of each value, arranged by byte position.
    Bits,
}

/// How the parts are compressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Codec {
    /// One zlib stream a part.
    Zlib,
    /// One Zstandard frame a part, with a window of at most 128 MiB.
    /// Written and read only by a build with the `zstd` feature. A
    /// header that names no codec means this one.
    #[default]
    Zstd,
}

#[cfg(not(feature = "zstd"))]
const NO_ZSTD: &str = "this build has no Zstandard: build with the `zstd` feature, or use zlib";

impl Codec {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "zlib" => Ok(Self::Zlib),
            "zstd" => Ok(Self::Zstd),
            other => Err(KuteError::InvalidConfig(format!("unknown codec {other:?}; use zstd or zlib"))),
        }
    }

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

    fn is_zstd(&self) -> bool {
        *self == Self::Zstd
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Absent in a container whose parts are Zstandard, so that those are
    /// the bytes the first release wrote.
    #[serde(default, skip_serializing_if = "Codec::is_zstd")]
    pub codec: Codec,
    pub values: u64,
    pub scheme: Scheme,
    /// `exact`, or `bounded`.
    pub contract: String,
    /// For `bounded`: no finite value is further than this from its original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_error: Option<f64>,
    /// The largest error any value actually has. Zero for `exact`.
    pub max_error_observed: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    /// For `steps`: the step, as the bits of an `f64`, so that a reader
    /// multiplies by the very same number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_bits: Option<u64>,
    /// How many times differences were taken (0 to 2).
    pub delta_order: u8,
    /// Values kept as raw bits beside the scheme.
    pub kept_exactly: u64,
    /// Bytes of the three parts as stored: main, positions of the values
    /// kept exactly, their bits.
    pub stored: [u64; 3],
    /// Bytes of the same parts before compression.
    pub raw: [u64; 3],
    /// SHA-256 of the values a reader obtains, eight little-endian bytes each.
    pub values_sha256: String,
}

/// What packing measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub header: Header,
    pub container_bytes: u64,
    /// Eight bytes a value.
    pub binary_bytes: u64,
    pub bits_per_value: f64,
}

fn damaged(why: impl Into<String>) -> KuteError {
    KuteError::Integrity(format!("signal container: {}", why.into()))
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

fn get_varint(bytes: &[u8], at: &mut usize) -> Result<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at).ok_or_else(|| damaged("a number runs past the end"))?;
        *at += 1;
        if shift == 63 && byte > 1 {
            return Err(damaged("a number is too large"));
        }
        n |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(n);
        }
    }
    Err(damaged("a number is too long"))
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn unzigzag(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

#[cfg_attr(not(feature = "zstd"), allow(dead_code))]
fn window_log(raw: u64) -> u32 {
    (u64::BITS - raw.saturating_sub(1).leading_zeros()).clamp(10, ZSTD_WINDOW_LOG)
}

fn compress(raw: &[u8], level: i32, codec: Codec) -> Result<Vec<u8>> {
    if codec == Codec::Zlib {
        // The levels asked for are Zstandard's: 19 for what is kept, 3 for a trial.
        let level = if level >= 19 { flate2::Compression::best() } else { flate2::Compression::new(4) };
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), level);
        encoder.write_all(raw)?;
        return Ok(encoder.finish()?);
    }
    zstd_compress(raw, level)
}

#[cfg(not(feature = "zstd"))]
fn zstd_compress(_: &[u8], _: i32) -> Result<Vec<u8>> {
    Err(KuteError::InvalidConfig(NO_ZSTD.into()))
}

#[cfg(feature = "zstd")]
fn zstd_compress(raw: &[u8], level: i32) -> Result<Vec<u8>> {
    let window = window_log(raw.len() as u64);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), level)?;
    encoder.window_log(window)?;
    encoder.long_distance_matching(window > 23)?;
    encoder.include_checksum(false)?;
    encoder.set_pledged_src_size(Some(raw.len() as u64))?;
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

/// Inflate exactly `raw` bytes from one frame, and nothing more.
fn expand(stored: &[u8], raw: u64, part: &str, codec: Codec) -> Result<Vec<u8>> {
    let bad = |why: String| damaged(format!("{part}: {why}"));
    let mut bytes = Vec::new();
    let spare = match codec {
        Codec::Zlib => {
            let mut decoder = flate2::read::ZlibDecoder::new(stored);
            decoder.by_ref().take(raw.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| bad(e.to_string()))?;
            stored.len() as u64 - decoder.total_in()
        }
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => return Err(KuteError::InvalidConfig(format!("{part}: the container is Zstandard; {NO_ZSTD}"))),
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(stored).map_err(|e| bad(e.to_string()))?.single_frame();
            decoder.window_log_max(window_log(raw)).map_err(|e| bad(e.to_string()))?;
            decoder.by_ref().take(raw.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| bad(e.to_string()))?;
            decoder.finish().len() as u64
        }
    };
    if bytes.len() as u64 != raw {
        return Err(bad("does not inflate to its declared size".into()));
    }
    if spare != 0 {
        return Err(bad("has bytes after its compressed data".into()));
    }
    Ok(bytes)
}

/// The value a reader makes of whole number `k`.
fn rebuild(k: i64, scheme: Scheme, scale: f64) -> f64 {
    match scheme {
        Scheme::Decimal => k as f64 / scale,
        _ => k as f64 * scale,
    }
}

/// Differences taken `order` times, in place. Wrapping, so always reversible.
fn difference(values: &mut [i64], order: u8) {
    for _ in 0..order {
        let mut previous = 0i64;
        for v in values.iter_mut() {
            let current = *v;
            *v = current.wrapping_sub(previous);
            previous = current;
        }
    }
}

fn integrate(values: &mut [i64], order: u8) {
    for _ in 0..order {
        let mut sum = 0i64;
        for v in values.iter_mut() {
            sum = sum.wrapping_add(*v);
            *v = sum;
        }
    }
}

fn varints(values: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for &v in values {
        put_varint(&mut out, zigzag(v));
    }
    out
}

fn hash(values: &[f64]) -> String {
    let mut hasher = Sha256::new();
    for v in values {
        hasher.update(v.to_bits().to_le_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The fewest decimal places that serve every value that is a decimal
/// of at most nine places. Values that are not (a third, a NaN) do not
/// count toward it and are kept beside the scheme.
fn decimals_of(values: &[f64]) -> u32 {
    let mut most = 0u32;
    for &v in values {
        if !v.is_finite() {
            continue;
        }
        let mut scale = 10f64.powi(most as i32);
        for d in most..=MAX_DECIMALS {
            let k = (v * scale).round();
            if k.abs() < MAX_WHOLE && (k / scale).to_bits() == v.to_bits() {
                most = d;
                break;
            }
            scale *= 10.0;
        }
    }
    most
}

struct Whole {
    scheme: Scheme,
    scale: f64,
    numbers: Vec<i64>,
    /// (position, bits) of the values the scheme cannot give back.
    kept: Vec<(u64, u64)>,
    max_error: f64,
}

/// Turn the values into whole numbers under one scheme, keeping beside
/// it every value the scheme would not give back well enough.
fn whole(values: &[f64], scheme: Scheme, scale: f64, tolerance: f64) -> Whole {
    let mut out = Whole { scheme, scale, numbers: Vec::with_capacity(values.len()), kept: Vec::new(), max_error: 0.0 };
    let mut last = 0i64;
    for (i, &v) in values.iter().enumerate() {
        let k = if scheme == Scheme::Decimal { (v * scale).round() } else { (v / scale).round() };
        let fits = v.is_finite() && k.abs() < MAX_WHOLE;
        let back = if fits { rebuild(k as i64, scheme, scale) } else { f64::NAN };
        let good = fits && if tolerance == 0.0 { back.to_bits() == v.to_bits() } else { (back - v).abs() <= tolerance };
        if good {
            last = k as i64;
            out.max_error = out.max_error.max((back - v).abs());
        } else {
            // In the stream it repeats the value before, which costs least.
            out.kept.push((i as u64, v.to_bits()));
        }
        out.numbers.push(last);
    }
    out
}

fn assemble(header: &Header, parts: [&[u8]; 3]) -> Result<Vec<u8>> {
    let head = serde_json::to_vec(header)?;
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + head.len() + parts.iter().map(|p| p.len()).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(head.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    for part in parts {
        out.extend_from_slice(part);
    }
    Ok(out)
}

pub fn pack(values: &[f64], mode: Mode) -> Result<(Vec<u8>, Report)> {
    pack_with(values, mode, Codec::preferred())
}

/// [`pack`] with the codec chosen. `zlib` writes a container that needs
/// nothing but a zlib reader; `zstd` needs a build with the `zstd`
/// feature, here and in the reader.
pub fn pack_with(values: &[f64], mode: Mode, codec: Codec) -> Result<(Vec<u8>, Report)> {
    if values.len() as u64 > MAX_VALUES {
        return Err(KuteError::LimitExceeded(format!("a signal container holds at most {MAX_VALUES} values")));
    }
    match mode {
        Mode::Exact => pack_as(values, mode, codec, true),
        Mode::Bounded { max_error } => {
            if !(max_error.is_finite() && max_error > 0.0) {
                return Err(KuteError::InvalidConfig("max_error must be a positive number".into()));
            }
            // Exact values keep any bound. When the bound is finer than the
            // signal's own resolution, steps of it cost more than the
            // values themselves; the smaller of the two is written.
            let exact = pack_as(values, mode, codec, true)?;
            // A bound so large that twice it is not a number has no step.
            // Exact values keep it.
            if !(2.0 * max_error).is_finite() {
                return Ok(exact);
            }
            let stepped = pack_as(values, mode, codec, false)?;
            Ok(if exact.0.len() < stepped.0.len() { exact } else { stepped })
        }
    }
}

/// One layout: the values as they are (`as_they_are`), or in steps of the bound.
fn pack_as(values: &[f64], mode: Mode, codec: Codec, as_they_are: bool) -> Result<(Vec<u8>, Report)> {
    let (contract, stated) = match mode {
        Mode::Exact => ("exact", None),
        Mode::Bounded { max_error } => ("bounded", Some(max_error)),
    };
    let (tolerance, plan) = match stated {
        Some(max_error) if !as_they_are => (max_error, (Scheme::Steps, 2.0 * max_error, 0)),
        _ => {
            let d = decimals_of(values);
            (0.0, (Scheme::Decimal, 10f64.powi(d as i32), d))
        }
    };
    let made = Some((whole(values, plan.0, plan.1, tolerance), plan.2));
    // Kept as they are, and too much of the signal is no decimal: store the bytes.
    // Values that are not finite (a missing reading) do not count: they are cheap beside.
    let made = made.filter(|(w, _)| !as_they_are || w.kept.iter().filter(|(_, bits)| f64::from_bits(*bits).is_finite()).count() <= (values.len() / 16).max(16));
    let (header, parts) = match made {
        None => {
            let mut planes = Vec::with_capacity(values.len() * 8);
            for byte in 0..8 {
                planes.extend(values.iter().map(|v| v.to_bits().to_le_bytes()[byte]));
            }
            let stored = compress(&planes, 19, codec)?;
            // The two parts this scheme does not use are still frames, empty ones.
            let empty = compress(&[], 19, codec)?;
            let header = Header {
                codec,
                values: values.len() as u64,
                scheme: Scheme::Bits,
                contract: contract.into(),
                max_error: stated,
                max_error_observed: 0.0,
                decimals: None,
                step_bits: None,
                delta_order: 0,
                kept_exactly: 0,
                stored: [stored.len() as u64, empty.len() as u64, empty.len() as u64],
                raw: [planes.len() as u64, 0, 0],
                values_sha256: hash(values),
            };
            (header, [stored, empty.clone(), empty])
        }
        Some((w, d)) => {
            // The order of differences that stores smallest, by trial.
            let mut best: Option<(usize, u8, Vec<u8>)> = None;
            for order in 0..=2u8 {
                let mut numbers = w.numbers.clone();
                difference(&mut numbers, order);
                let raw = varints(&numbers);
                let trial = compress(&raw, 3, codec)?.len();
                if best.as_ref().is_none_or(|(size, _, _)| trial < *size) {
                    best = Some((trial, order, raw));
                }
            }
            let (_, order, raw_main) = best.expect("three orders tried");
            let (mut positions, mut bits) = (Vec::new(), Vec::with_capacity(w.kept.len() * 8));
            let mut previous = 0u64;
            for &(at, value) in &w.kept {
                put_varint(&mut positions, at - previous);
                previous = at;
                bits.extend_from_slice(&value.to_le_bytes());
            }
            let parts = [compress(&raw_main, 19, codec)?, compress(&positions, 19, codec)?, compress(&bits, 19, codec)?];
            // What a reader will obtain: hash that, not what was given.
            let mut obtained: Vec<f64> = w.numbers.iter().map(|&k| rebuild(k, w.scheme, w.scale)).collect();
            for &(at, value) in &w.kept {
                obtained[at as usize] = f64::from_bits(value);
            }
            let header = Header {
                codec,
                values: values.len() as u64,
                scheme: w.scheme,
                contract: contract.into(),
                max_error: stated,
                max_error_observed: w.max_error,
                decimals: (w.scheme == Scheme::Decimal).then_some(d),
                step_bits: (w.scheme == Scheme::Steps).then_some(w.scale.to_bits()),
                delta_order: order,
                kept_exactly: w.kept.len() as u64,
                stored: [parts[0].len() as u64, parts[1].len() as u64, parts[2].len() as u64],
                raw: [raw_main.len() as u64, positions.len() as u64, bits.len() as u64],
                values_sha256: hash(&obtained),
            };
            (header, parts)
        }
    };
    let container = assemble(&header, [&parts[0], &parts[1], &parts[2]])?;
    // Read it back before saying it is good.
    let back = unpack(&container, values.len() as u64)?;
    for (given, got) in values.iter().zip(&back) {
        let fine = match mode {
            Mode::Exact => given.to_bits() == got.to_bits(),
            Mode::Bounded { max_error } => given.to_bits() == got.to_bits() || (given - got).abs() <= max_error,
        };
        if !fine {
            return Err(KuteError::Backend("the container does not keep its contract; nothing was written".into()));
        }
    }
    let report = Report {
        container_bytes: container.len() as u64,
        binary_bytes: values.len() as u64 * 8,
        bits_per_value: if values.is_empty() { 0.0 } else { container.len() as f64 * 8.0 / values.len() as f64 },
        header,
    };
    Ok((container, report))
}

/// The header and where the parts begin.
fn open(container: &[u8]) -> Result<(Header, usize)> {
    if container.len() < MAGIC.len() + 4 || &container[..MAGIC.len()] != MAGIC {
        return Err(damaged("not a KUTE signal container"));
    }
    let length = u32::from_le_bytes(container[MAGIC.len()..MAGIC.len() + 4].try_into().expect("four bytes")) as usize;
    let start = MAGIC.len() + 4;
    if length > MAX_HEADER_BYTES || container.len() - start < length {
        return Err(damaged("the header is too long or cut short"));
    }
    let header: Header = serde_json::from_slice(&container[start..start + length]).map_err(|e| damaged(format!("the header does not parse: {e}")))?;
    // What the header says of itself is read by people and programs
    // deciding whether to trust the values, and the hash does not cover
    // it. A header whose statements cannot all be true is refused.
    let observed = header.max_error_observed;
    let agrees = match (header.contract.as_str(), header.max_error) {
        ("exact", None) => observed == 0.0 && header.scheme != Scheme::Steps,
        ("bounded", Some(bound)) => bound.is_finite() && bound > 0.0 && (0.0..=bound).contains(&observed),
        _ => false,
    };
    let fields = match header.scheme {
        Scheme::Decimal => header.decimals.is_some() && header.step_bits.is_none(),
        Scheme::Steps => header.decimals.is_none() && header.step_bits.is_some(),
        Scheme::Bits => header.decimals.is_none() && header.step_bits.is_none() && observed == 0.0,
    };
    if !agrees || !fields {
        return Err(damaged("the header's statements about its contract do not agree with each other"));
    }
    Ok((header, start + length))
}

/// Read the header alone.
pub fn header(container: &[u8]) -> Result<Header> {
    Ok(open(container)?.0)
}

/// The values, or an error: a container that would give other values
/// than its header promises is refused. `max_values` bounds the memory a
/// container can ask for.
pub fn unpack(container: &[u8], max_values: u64) -> Result<Vec<f64>> {
    let (header, mut at) = open(container)?;
    let n = header.values;
    if n > max_values.min(MAX_VALUES) {
        return Err(KuteError::LimitExceeded(format!("the container holds {n} values; the limit given is {max_values}")));
    }
    let count = n as usize;
    // Each part has a size the header cannot exceed for this many values.
    let kept = header.kept_exactly;
    let limits = match header.scheme {
        Scheme::Bits => [n * 8, 0, 0],
        _ => [n * 10, kept * 10, kept * 8],
    };
    if kept > n || header.delta_order > 2 || (header.scheme == Scheme::Bits && (kept != 0 || header.raw[0] != n * 8)) || header.raw[2] != kept * 8 {
        return Err(damaged("the header contradicts itself"));
    }
    let mut parts: [Vec<u8>; 3] = Default::default();
    for i in 0..3 {
        if header.raw[i] > limits[i] {
            return Err(damaged("a part is larger than its values allow"));
        }
        let size = usize::try_from(header.stored[i]).ok().filter(|s| *s <= container.len() - at).ok_or_else(|| damaged("a part runs past the end"))?;
        parts[i] = expand(&container[at..at + size], header.raw[i], ["values", "positions", "kept values"][i], header.codec)?;
        at += size;
    }
    if at != container.len() {
        return Err(damaged("bytes after the last part"));
    }
    // Every value takes at least one byte of the first part. Checked
    // before anything is set aside for the values: a header that claims
    // two thousand million values around a few bytes would otherwise ask
    // for 16 GB, which a machine short of memory refuses by ending the
    // process.
    if parts[0].len() < count {
        return Err(damaged("fewer numbers than values"));
    }
    let mut values = Vec::with_capacity(count);
    match header.scheme {
        Scheme::Bits => {
            for i in 0..count {
                let mut bytes = [0u8; 8];
                for (plane, byte) in bytes.iter_mut().enumerate() {
                    *byte = parts[0][plane * count + i];
                }
                values.push(f64::from_bits(u64::from_le_bytes(bytes)));
            }
        }
        scheme => {
            let scale = match scheme {
                Scheme::Decimal => {
                    let d = header.decimals.filter(|d| *d <= MAX_DECIMALS).ok_or_else(|| damaged("no decimal scale"))?;
                    10f64.powi(d as i32)
                }
                _ => {
                    let step = f64::from_bits(header.step_bits.ok_or_else(|| damaged("no step"))?);
                    if !(step.is_finite() && step > 0.0) {
                        return Err(damaged("the step is not a positive number"));
                    }
                    step
                }
            };
            let mut numbers = Vec::with_capacity(count);
            let mut read = 0usize;
            for _ in 0..count {
                numbers.push(unzigzag(get_varint(&parts[0], &mut read)?));
            }
            if read != parts[0].len() {
                return Err(damaged("more numbers than values"));
            }
            integrate(&mut numbers, header.delta_order);
            values.extend(numbers.iter().map(|&k| rebuild(k, scheme, scale)));
            let (mut read, mut position) = (0usize, 0u64);
            for (i, bits) in parts[2].chunks_exact(8).enumerate() {
                let step = get_varint(&parts[1], &mut read)?;
                position = position.checked_add(step).filter(|p| *p < n && (i == 0 || step > 0)).ok_or_else(|| damaged("a kept value is out of place"))?;
                values[position as usize] = f64::from_bits(u64::from_le_bytes(bits.try_into().expect("eight bytes")));
            }
            if read != parts[1].len() {
                return Err(damaged("more positions than kept values"));
            }
        }
    }
    if hash(&values) != header.values_sha256 {
        return Err(damaged("the values are not the ones the header promises"));
    }
    Ok(values)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_of_any_size_come_back_and_no_more_than_declared() {
        for codec in [Codec::Zlib, Codec::Zstd].into_iter().filter(|c| c.available()) {
            for raw in [vec![], vec![7u8, 1, 1, 1], vec![0; 32], (0..5000).map(|i| (i * 7) as u8).collect()] {
                let mut stored = compress(&raw, 19, codec).unwrap();
                assert_eq!(expand(&stored, raw.len() as u64, "part", codec).unwrap(), raw);
                if !raw.is_empty() {
                    assert!(expand(&stored, raw.len() as u64 - 1, "part", codec).is_err());
                }
                assert!(expand(&stored, raw.len() as u64 + 1, "part", codec).is_err());
                stored.push(0);
                assert!(expand(&stored, raw.len() as u64, "part", codec).is_err(), "bytes after the data");
            }
            assert!(expand(&[], 0, "part", codec).is_err());
        }
        if !Codec::Zstd.available() {
            assert!(compress(b"abc", 19, Codec::Zstd).unwrap_err().to_string().contains("zstd"));
            assert!(expand(b"abc", 3, "part", Codec::Zstd).unwrap_err().to_string().contains("zstd"));
        }
    }

    #[test]
    fn decimal_places_are_found_past_values_that_have_none() {
        assert_eq!(decimals_of(&[1.0, 2.5, 0.125]), 3);
        assert_eq!(decimals_of(&[1.0, 1.0 / 3.0, 2.5, f64::NAN, -0.0]), 1);
        assert_eq!(decimals_of(&[]), 0);
        assert_eq!(decimals_of(&[1e300, 4.0]), 0);
    }
}
