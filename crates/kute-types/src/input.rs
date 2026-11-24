use crate::{KuteError, Result, Version, VocabularyId};
use serde::{Deserialize, Serialize};

/// What kind of information a payload carries. Declared by the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Binary,
    Structured,
    Events,
    Other,
}

/// The two canonical shapes a tokenizer accepts (ADR-0004).
///
/// `Bytes` covers text, binary and serialized structured data. `Symbols` is an
/// ordered sequence of opaque names (events, opcodes, gates) for information
/// whose units are not bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Bytes(Vec<u8>),
    Symbols(Vec<String>),
}

impl Payload {
    pub fn kind(&self) -> PayloadKind {
        match self {
            Payload::Bytes(_) => PayloadKind::Bytes,
            Payload::Symbols(_) => PayloadKind::Symbols,
        }
    }

    /// Number of units: bytes or symbols.
    pub fn len(&self) -> usize {
        match self {
            Payload::Bytes(b) => b.len(),
            Payload::Symbols(s) => s.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Bytes,
    Symbols,
}

/// Adapter output and tokenizer input: one record of canonical information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalInput {
    pub modality: Modality,
    pub payload: Payload,
}

impl CanonicalInput {
    pub fn bytes(modality: Modality, bytes: impl Into<Vec<u8>>) -> Self {
        Self { modality, payload: Payload::Bytes(bytes.into()) }
    }

    pub fn text(text: &str) -> Self {
        Self::bytes(Modality::Text, text.as_bytes())
    }

    pub fn symbols<I, S>(symbols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            modality: Modality::Events,
            payload: Payload::Symbols(symbols.into_iter().map(Into::into).collect()),
        }
    }
}

/// Token ids scoped to the exact vocabulary that produced them.
///
/// Ids from different vocabularies are never interchangeable, so a sequence
/// always travels with the vocabulary id, version and content digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSequence {
    pub vocabulary_id: VocabularyId,
    pub vocabulary_version: Version,
    pub vocabulary_digest: String,
    pub modality: Modality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    pub ids: Vec<u32>,
}

#[derive(Serialize, Deserialize)]
struct ContainerHeader {
    vocabulary_id: VocabularyId,
    vocabulary_version: Version,
    vocabulary_digest: String,
    modality: Modality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter: Option<String>,
    count: u64,
}

const MAGIC: &[u8; 5] = b"KUTE\x01";
const MAX_HEADER: usize = 64 * 1024;

impl TokenSequence {
    /// Serialize to the `.kute` container: magic, little-endian header length,
    /// JSON header, then LEB128 varint token ids.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let header = serde_json::to_vec(&ContainerHeader {
            vocabulary_id: self.vocabulary_id.clone(),
            vocabulary_version: self.vocabulary_version.clone(),
            vocabulary_digest: self.vocabulary_digest.clone(),
            modality: self.modality,
            adapter: self.adapter.clone(),
            count: self.ids.len() as u64,
        })?;
        if header.len() > MAX_HEADER {
            return Err(KuteError::LimitExceeded("token sequence header exceeds 64 KiB".into()));
        }
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + header.len() + self.ids.len() * 2);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        for &id in &self.ids {
            let mut v = id;
            loop {
                let byte = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(byte);
                    break;
                }
                out.push(byte | 0x80);
            }
        }
        Ok(out)
    }

    /// Parse a `.kute` container. Rejects truncated, oversized or trailing data.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| KuteError::InvalidInput(format!("not a valid .kute token sequence: {why}"));
        if bytes.len() < MAGIC.len() + 4 || &bytes[..MAGIC.len()] != MAGIC {
            return Err(bad("missing KUTE magic"));
        }
        let mut pos = MAGIC.len();
        let header_len = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        pos += 4;
        if header_len > MAX_HEADER {
            return Err(bad("header longer than 64 KiB"));
        }
        let header_end = pos.checked_add(header_len).filter(|&e| e <= bytes.len()).ok_or_else(|| bad("truncated header"))?;
        let header: ContainerHeader = serde_json::from_slice(&bytes[pos..header_end]).map_err(|e| bad(&format!("header: {e}")))?;
        pos = header_end;
        let remaining = bytes.len() - pos;
        if header.count > remaining as u64 {
            return Err(bad("declared token count exceeds the data present"));
        }
        let mut ids = Vec::with_capacity(header.count as usize);
        while pos < bytes.len() {
            let mut value: u32 = 0;
            let mut shift = 0u32;
            loop {
                let byte = *bytes.get(pos).ok_or_else(|| bad("truncated token id"))?;
                pos += 1;
                let low = (byte & 0x7f) as u32;
                if shift == 28 && low > 0x0f {
                    return Err(bad("token id overflows 32 bits"));
                }
                value |= low << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
                if shift > 28 {
                    return Err(bad("token id overflows 32 bits"));
                }
            }
            ids.push(value);
        }
        if ids.len() as u64 != header.count {
            return Err(bad("token count does not match the header"));
        }
        Ok(Self {
            vocabulary_id: header.vocabulary_id,
            vocabulary_version: header.vocabulary_version,
            vocabulary_digest: header.vocabulary_digest,
            modality: header.modality,
            adapter: header.adapter,
            ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(ids: Vec<u32>) -> TokenSequence {
        TokenSequence {
            vocabulary_id: VocabularyId::parse("kute:test").unwrap(),
            vocabulary_version: Version::new(1, 0, 0),
            vocabulary_digest: "sha256:00".into(),
            modality: Modality::Text,
            adapter: Some("text".into()),
            ids,
        }
    }

    #[test]
    fn container_round_trip() {
        let s = seq(vec![0, 1, 127, 128, 255, 16_384, 1_048_576, u32::MAX]);
        let bytes = s.to_bytes().unwrap();
        assert_eq!(TokenSequence::from_bytes(&bytes).unwrap(), s);
        let empty = seq(vec![]);
        assert_eq!(TokenSequence::from_bytes(&empty.to_bytes().unwrap()).unwrap(), empty);
    }

    #[test]
    fn container_rejects_damage() {
        let bytes = seq(vec![1, 2, 300, 70_000]).to_bytes().unwrap();
        for cut in 0..bytes.len() {
            assert!(TokenSequence::from_bytes(&bytes[..cut]).is_err(), "truncated at {cut}");
        }
        let mut extra = bytes.clone();
        extra.push(0x05);
        assert!(TokenSequence::from_bytes(&extra).is_err());
        let mut overflow = seq(vec![]).to_bytes().unwrap();
        overflow.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x7f]);
        assert!(TokenSequence::from_bytes(&overflow).is_err());
        assert!(TokenSequence::from_bytes(b"not a kute file").is_err());
    }
}
