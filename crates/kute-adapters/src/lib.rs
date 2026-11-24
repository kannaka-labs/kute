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

fn parse(bytes: &[u8], what: &str) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|e| KuteError::InvalidInput(format!("{what}: {e}")))
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
    fn ingest(&self, input: &[u8], _: &AdapterConfig) -> Result<CanonicalInput> {
        Ok(CanonicalInput::bytes(Modality::Structured, canonical(&parse(input, "json adapter")?)?))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, JSON)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(JSON, Modality::Structured, false, "One JSON document, re-serialized with sorted keys and no extra whitespace. Original formatting does not survive. Numbers that fit a 64-bit integer or a double survive; larger or more precise ones are rounded to a double. When an object repeats a key, the last value is kept.")
    }
}

pub struct JsonlAdapter;

impl ModalityAdapter for JsonlAdapter {
    fn modality(&self) -> Modality {
        Modality::Structured
    }
    fn ingest(&self, input: &[u8], _: &AdapterConfig) -> Result<CanonicalInput> {
        let mut out = Vec::with_capacity(input.len());
        for (n, line) in input.split(|&b| b == b'\n').enumerate() {
            let line = trim(line);
            if line.is_empty() {
                continue;
            }
            out.extend_from_slice(&canonical(&parse(line, &format!("jsonl adapter, line {}", n + 1))?)?);
            out.push(b'\n');
        }
        Ok(CanonicalInput::bytes(Modality::Structured, out))
    }
    fn reconstruct(&self, input: &CanonicalInput, _config: &AdapterConfig) -> Result<Vec<u8>> {
        bytes_of(input, JSONL)
    }
    fn capabilities(&self) -> AdapterCapabilities {
        caps(JSONL, Modality::Structured, false, "One JSON value per line, each canonicalized; blank lines dropped. Original formatting does not survive. Numbers that fit a 64-bit integer or a double survive; larger or more precise ones are rounded to a double. When an object repeats a key, the last value is kept.")
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
            let value = parse(line, &what)?;
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
        caps(JSONL_EVENTS, Modality::Events, false, "Extracts one field per JSONL record as an event symbol (option: field, default \"event\"). All other fields are dropped.")
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
