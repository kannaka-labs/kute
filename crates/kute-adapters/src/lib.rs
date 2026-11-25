//! Modality adapters (FR-001). Each one declares whether it reconstructs the
//! original bytes exactly; that flag is copied into every vocabulary trained
//! through it, so a reader can tell lossless pipelines from lossy ones.
//!
//! | name           | output  | byte-exact | what it does |
//! |----------------|---------|------------|--------------|
//! | `binary`       | bytes   | yes        | passes bytes through |
//! | `text`         | bytes   | yes        | requires valid UTF-8, passes it through |
//! | `json`         | bytes   | no         | parses one JSON document and re-serializes it canonically |
//! | `jsonl`        | bytes   | no         | canonicalizes each non-empty line |
//! | `jsonl-events` | symbols | no         | takes one field per line as an event symbol |
//!
//! The JSON adapters keep every value but not the original spacing or key
//! order: that is the point of canonicalizing, and it is declared as lossy at
//! the byte level. To keep a JSON file byte for byte, use `text` or `binary`.
//!
//! They are strict: a document in which an object repeats a key, or which
//! holds a number that would not survive as written (an integer beyond 64
//! bits, a decimal with more digits than a double keeps), is refused, since
//! canonicalizing it would silently pick one reading. The adapter option
//! `strict=false` restores the earlier behaviour: last key wins, numbers are
//! rounded.

use kute_core::{AdapterCapabilities, ModalityAdapter};
use kute_types::{AdapterConfig, CanonicalInput, KuteError, Modality, Payload, Result};
use serde_json::Value;

pub const BINARY: &str = "binary";
pub const TEXT: &str = "text";
pub const JSON: &str = "json";
pub const JSONL: &str = "jsonl";
pub const JSONL_EVENTS: &str = "jsonl-events";

/// Register this crate's adapters.
pub fn register(registry: &mut kute_core::Registry) -> kute_types::Result<()> {
    registry.register_adapter(BINARY, || Box::new(BinaryAdapter))?;
    registry.register_adapter(TEXT, || Box::new(TextAdapter))?;
    registry.register_adapter(JSON, || Box::new(JsonAdapter))?;
    registry.register_adapter(JSONL, || Box::new(JsonlAdapter))?;
    registry.register_adapter(JSONL_EVENTS, || Box::new(JsonlEventsAdapter))
}

fn bytes_of(input: &CanonicalInput, adapter: &str) -> Result<Vec<u8>> {
    match &input.payload {
        Payload::Bytes(b) => Ok(b.clone()),
        Payload::Symbols(_) => Err(KuteError::InvalidInput(format!("the {adapter} adapter reconstructs byte payloads, not symbols"))),
    }
}

fn caps(name: &str, modality: Modality, lossless: bool, notes: &str) -> AdapterCapabilities {
    AdapterCapabilities { name: name.to_string(), modality, lossless, notes: notes.to_string() }
}

/// Canonical JSON: object keys sorted, no insignificant whitespace.
fn canonical(value: &Value) -> Result<Vec<u8>> {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), sort(&map[k]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    Ok(serde_json::to_vec(&sort(value))?)
}

fn parse(bytes: &[u8], what: &str, config: &AdapterConfig) -> Result<Value> {
    let value = serde_json::from_slice(bytes).map_err(|e| KuteError::InvalidInput(format!("{what}: {e}")))?;
    match config.get("strict") {
        None | Some("true") => strict_check(bytes).map_err(|e| KuteError::InvalidInput(format!("{what}: {e} (the adapter option strict=false accepts it)")))?,
        Some("false") => {}
        Some(other) => return Err(KuteError::InvalidInput(format!("{what}: option strict must be true or false, not {other:?}"))),
    }
    Ok(value)
}

/// Refuse what canonicalizing would silently change: repeated keys and
/// numbers that do not survive. `bytes` has already parsed as JSON, so the
/// scan only has to find keys and numbers, not validate.
fn strict_check(bytes: &[u8]) -> std::result::Result<(), String> {
    // One entry per open container: the keys seen so far if it is an object.
    let mut open: Vec<Option<std::collections::HashSet<String>>> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => open.push(Some(Default::default())),
            b'[' => open.push(None),
            b'}' | b']' => {
                open.pop();
            }
            b'"' => {
                let start = i;
                i += 1;
                while bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                let mut after = i + 1;
                while after < bytes.len() && bytes[after].is_ascii_whitespace() {
                    after += 1;
                }
                if bytes.get(after) == Some(&b':') {
                    if let Some(Some(keys)) = open.last_mut() {
                        // Compare keys as decoded: "a" and "a" are one key.
                        let key: String = serde_json::from_slice(&bytes[start..=i]).map_err(|e| e.to_string())?;
                        if keys.contains(&key) {
                            return Err(format!("an object repeats the key {key:?}"));
                        }
                        keys.insert(key);
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                // Valid JSON numbers are ASCII.
                check_number(std::str::from_utf8(&bytes[start..i]).map_err(|e| e.to_string())?)?;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// A decimal literal as its significant digits and the power of ten of the
/// last one, so that `2.50`, `25e-1` and `2.5` compare equal.
fn decimal(literal: &str) -> Option<(String, i64)> {
    let literal = literal.trim_start_matches('-');
    let (mantissa, exponent) = match literal.find(['e', 'E']) {
        Some(at) => (&literal[..at], literal[at + 1..].parse::<i64>().ok()?),
        None => (literal, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}");
    let mut power = exponent.checked_sub(fraction.len() as i64)?;
    while digits.ends_with('0') {
        digits.pop();
        power = power.checked_add(1)?;
    }
    let digits = digits.trim_start_matches('0').to_string();
    Some(if digits.is_empty() { (digits, 0) } else { (digits, power) })
}

fn check_number(literal: &str) -> std::result::Result<(), String> {
    if !literal.contains(['.', 'e', 'E']) {
        return if literal.parse::<i64>().is_ok() || literal.parse::<u64>().is_ok() {
            Ok(())
        } else {
            Err(format!("the integer {literal} does not fit 64 bits and would be rounded"))
        };
    }
    let rounded = || format!("the number {literal} has more precision than a double keeps and would be rounded");
    let as_double: f64 = literal.parse().map_err(|_| rounded())?;
    // What the canonical form will hold, compared digit for digit.
    let written = serde_json::Number::from_f64(as_double).ok_or_else(rounded)?.to_string();
    if decimal(literal).is_some() && decimal(literal) == decimal(&written) {
        Ok(())
    } else {
        Err(rounded())
    }
}

pub struct BinaryAdapter;

impl ModalityAdapter for BinaryAdapter {
    fn modality(&self) -> Modality {
        Modality::Binary
    }
    fn ingest(&self, input: &[u8], _: &AdapterConfig) -> Result<CanonicalInput> {
        Ok(CanonicalInput::bytes(Modality::Binary, input))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, BINARY)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(BINARY, Modality::Binary, true, "Arbitrary bytes, passed through unchanged.")
    }
}

pub struct TextAdapter;

impl ModalityAdapter for TextAdapter {
    fn modality(&self) -> Modality {
        Modality::Text
    }
    fn ingest(&self, input: &[u8], _: &AdapterConfig) -> Result<CanonicalInput> {
        std::str::from_utf8(input).map_err(|e| KuteError::InvalidInput(format!("text adapter: not valid UTF-8 ({e}); use the binary adapter")))?;
        Ok(CanonicalInput::bytes(Modality::Text, input))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, TEXT)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(TEXT, Modality::Text, true, "Valid UTF-8, passed through unchanged. No normalization.")
    }
}

pub struct JsonAdapter;

impl ModalityAdapter for JsonAdapter {
    fn modality(&self) -> Modality {
        Modality::Structured
    }
    fn ingest(&self, input: &[u8], config: &AdapterConfig) -> Result<CanonicalInput> {
        Ok(CanonicalInput::bytes(Modality::Structured, canonical(&parse(input, "json adapter", config)?)?))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, JSON)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(JSON, Modality::Structured, false, "One JSON document, re-serialized with sorted keys and no extra whitespace. Original formatting does not survive. Strict by default: a repeated key, an integer beyond 64 bits or a decimal a double would round is refused. With the option strict=false the last value of a repeated key is kept and such numbers are rounded.")
    }
}

pub struct JsonlAdapter;

impl ModalityAdapter for JsonlAdapter {
    fn modality(&self) -> Modality {
        Modality::Structured
    }
    fn ingest(&self, input: &[u8], config: &AdapterConfig) -> Result<CanonicalInput> {
        let mut out = Vec::with_capacity(input.len());
        for (n, line) in input.split(|&b| b == b'\n').enumerate() {
            let line = trim(line);
            if line.is_empty() {
                continue;
            }
            out.extend_from_slice(&canonical(&parse(line, &format!("jsonl adapter, line {}", n + 1), config)?)?);
            out.push(b'\n');
        }
        Ok(CanonicalInput::bytes(Modality::Structured, out))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, JSONL)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(JSONL, Modality::Structured, false, "One JSON value per line, each canonicalized; blank lines dropped. Original formatting does not survive. Strict by default: a repeated key, an integer beyond 64 bits or a decimal a double would round is refused. With the option strict=false the last value of a repeated key is kept and such numbers are rounded.")
    }
}

/// Reads one field from each JSONL record as an event symbol.
///
/// Options: `field` (default `event`). Reconstruction writes one
/// `{"<field>": "<symbol>"}` line per symbol, so everything else in the
/// original records is gone: this adapter is for learning event vocabularies,
/// not for archiving logs.
#[derive(Default)]
pub struct JsonlEventsAdapter;

const DEFAULT_FIELD: &str = "event";

impl ModalityAdapter for JsonlEventsAdapter {
    fn modality(&self) -> Modality {
        Modality::Events
    }
    fn ingest(&self, input: &[u8], config: &AdapterConfig) -> Result<CanonicalInput> {
        let field = config.get("field").unwrap_or(DEFAULT_FIELD);
        let mut symbols = Vec::new();
        for (n, line) in input.split(|&b| b == b'\n').enumerate() {
            let line = trim(line);
            if line.is_empty() {
                continue;
            }
            let what = format!("jsonl-events adapter, line {}", n + 1);
            let value = parse(line, &what, config)?;
            let symbol = match value.get(field) {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                Some(Value::Number(num)) => num.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(_) => return Err(KuteError::InvalidInput(format!("{what}: field {field:?} must be a non-empty string, number or boolean"))),
                None => return Err(KuteError::InvalidInput(format!("{what}: no field {field:?}"))),
            };
            if symbol.len() > kute_types::vocab::MAX_SYMBOL_BYTES {
                return Err(KuteError::InvalidInput(format!("{what}: symbol longer than 1024 bytes")));
            }
            symbols.push(symbol);
        }
        Ok(CanonicalInput { modality: Modality::Events, payload: Payload::Symbols(symbols) })
    }
    fn reconstruct(&self, input: &CanonicalInput, config: &AdapterConfig) -> Result<Vec<u8>> {
        let field = config.get("field").unwrap_or(DEFAULT_FIELD);
        let Payload::Symbols(symbols) = &input.payload else {
            return Err(KuteError::InvalidInput("the jsonl-events adapter reconstructs symbol payloads".into()));
        };
        let mut out = Vec::new();
        for s in symbols {
            out.extend_from_slice(&serde_json::to_vec(&serde_json::json!({ field: s }))?);
            out.push(b'\n');
        }
        Ok(out)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(JSONL_EVENTS, Modality::Events, false, "Extracts one field per JSONL record as an event symbol (options: field, default \"event\"; strict, default true, which refuses records with a repeated key or a number that would be rounded). All other fields are dropped.")
    }
}

fn trim(mut line: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = line {
        if first.is_ascii_whitespace() { line = rest } else { break }
    }
    while let [rest @ .., last] = line {
        if last.is_ascii_whitespace() { line = rest } else { break }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_through_adapters_are_exact() {
        let cfg = AdapterConfig::default();
        let data: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
        let c = BinaryAdapter.ingest(&data, &cfg).unwrap();
        assert_eq!(BinaryAdapter.reconstruct(&c, &AdapterConfig::default()).unwrap(), data);
        assert!(BinaryAdapter.capabilities().lossless);
        let text = "naïve café — 日本語\r\n";
        let c = TextAdapter.ingest(text.as_bytes(), &cfg).unwrap();
        assert_eq!(TextAdapter.reconstruct(&c, &AdapterConfig::default()).unwrap(), text.as_bytes());
        assert!(TextAdapter.ingest(&[0xff, 0xfe], &cfg).is_err());
    }

    #[test]
    fn json_is_canonical_and_declared_lossy() {
        let cfg = AdapterConfig::default();
        let a = JsonAdapter.ingest(br#"{ "b": [1, 2.50, {"z":null,"a":true}], "a": "x" }"#, &cfg).unwrap();
        let b = JsonAdapter.ingest(br#"{"a":"x","b":[1,2.50,{"a":true,"z":null}]}"#, &cfg).unwrap();
        assert_eq!(a, b);
        assert_eq!(JsonAdapter.reconstruct(&a, &AdapterConfig::default()).unwrap(), br#"{"a":"x","b":[1,2.5,{"a":true,"z":null}]}"#);
        assert!(!JsonAdapter.capabilities().lossless);
        assert!(JsonAdapter.ingest(b"{not json", &cfg).is_err());
    }

    #[test]
    fn strict_json_refuses_what_canonicalizing_would_change() {
        let strict = AdapterConfig::default();
        let loose = AdapterConfig::default().with("strict", "false");
        let refused = [
            (r#"{"a":1,"a":2}"#, "repeats the key"),
            (r#"{"a":1,"a":2}"#, "repeats the key"),
            (r#"[{"x":{"k":1,"k":1}}]"#, "repeats the key"),
            ("18446744073709551616", "does not fit 64 bits"),
            ("-9223372036854775809", "does not fit 64 bits"),
            ("0.1000000000000000055", "more precision"),
            ("[1.00000000000000001]", "more precision"),
            ("123456789012345678.5", "more precision"),
            ("1e-400", "more precision"),
        ];
        for (text, why) in refused {
            let err = JsonAdapter.ingest(text.as_bytes(), &strict).unwrap_err().to_string();
            assert!(err.contains(why) && err.contains("strict=false"), "{text}: {err}");
            assert!(JsonAdapter.ingest(text.as_bytes(), &loose).is_ok(), "{text}");
        }
        let accepted = [
            r#"{"a":1,"b":{"a":2},"c":[{"a":3},{"a":4}]}"#,
            r#"{"a:":"a","k":"\"a\":1,\"a\":2","s":"{\\"}"#,
            "[0, -0, 2.50, 25e-1, 1e2, 1E+2, 0.1, 1.7976931348623157e308, 5e-324, 18446744073709551615, -9223372036854775808]",
            r#"["1e999", "00012345678901234567890"]"#,
            "0.30000000000000004",
        ];
        for text in accepted {
            let a = JsonAdapter.ingest(text.as_bytes(), &strict).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(a, JsonAdapter.ingest(text.as_bytes(), &loose).unwrap(), "{text}");
        }
        // Line numbers survive, the events adapter is covered, bad option values are named.
        let err = JsonlAdapter.ingest(b"{\"a\":1}\n{\"a\":1,\"a\":2}\n", &strict).unwrap_err().to_string();
        assert!(err.contains("line 2") && err.contains("repeats the key"), "{err}");
        assert!(JsonlEventsAdapter.ingest(b"{\"event\":\"A\",\"event\":\"B\"}\n", &strict).is_err());
        assert!(JsonlEventsAdapter.ingest(b"{\"event\":\"A\",\"event\":\"B\"}\n", &loose).is_ok());
        let err = JsonAdapter.ingest(b"1", &AdapterConfig::default().with("strict", "yes")).unwrap_err().to_string();
        assert!(err.contains("must be true or false"), "{err}");
    }

    #[test]
    fn jsonl_canonicalizes_each_line() {
        let cfg = AdapterConfig::default();
        let c = JsonlAdapter.ingest(b"{\"b\":1, \"a\":2}\r\n\n  {\"x\": [ ]}\n", &cfg).unwrap();
        assert_eq!(JsonlAdapter.reconstruct(&c, &AdapterConfig::default()).unwrap(), b"{\"a\":2,\"b\":1}\n{\"x\":[]}\n");
        let err = JsonlAdapter.ingest(b"{\"ok\":1}\n{broken\n", &cfg).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn events_extracts_a_field() {
        let cfg = AdapterConfig::default().with("field", "tag");
        let c = JsonlEventsAdapter.ingest(b"{\"tag\":\"MOTOR_START\",\"t\":1}\n{\"tag\":\"MOTOR_TRIP\",\"t\":2}\n", &cfg).unwrap();
        assert_eq!(c.payload, Payload::Symbols(vec!["MOTOR_START".into(), "MOTOR_TRIP".into()]));
        assert!(JsonlEventsAdapter.ingest(b"{\"other\":1}\n", &cfg).is_err());
        assert!(!JsonlEventsAdapter.capabilities().lossless);
    }

    #[test]
    fn events_come_back_under_the_field_they_were_read_from() {
        let config = AdapterConfig::default().with("field", "tag");
        let raw = b"{\"tag\":\"A\",\"other\":1}
{\"tag\":\"B\"}
";
        let c = JsonlEventsAdapter.ingest(raw, &config).unwrap();
        let out = JsonlEventsAdapter.reconstruct(&c, &config).unwrap();
        assert_eq!(out, b"{\"tag\":\"A\"}
{\"tag\":\"B\"}
");
        // The adapter can read its own output.
        assert_eq!(JsonlEventsAdapter.ingest(&out, &config).unwrap(), c);
    }
}
