use crate::{KuteError, Result, Version, VocabularyId};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

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
    /// Format 2 only: width of each id in the compressed body, 1 to 4 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_bytes: Option<u8>,
}

const MAGIC: &[u8; 5] = b"KUTE\x01";
/// Format 2: the ids are fixed-width little-endian integers in a zlib stream.
const MAGIC_COMPRESSED: &[u8; 5] = b"KUTE\x02";
const MAX_HEADER: usize = 64 * 1024;
/// DEFLATE cannot expand by more than about 1032 to 1, so a body that claims
/// more output than this for its size is rejected before any is produced.
const MAX_INFLATE_RATIO: u64 = 1040;
/// Largest buffer reserved on the word of a container header alone.
const MAX_PREALLOCATE: u64 = 1 << 20;

impl TokenSequence {
    /// Serialize to the `.kute` container: magic, little-endian header length,
    /// JSON header, then LEB128 varint token ids.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = self.start_container(MAGIC, None, self.ids.len() * 2)?;
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

    fn start_container(&self, magic: &[u8; 5], id_bytes: Option<u8>, body_hint: usize) -> Result<Vec<u8>> {
        let header = serde_json::to_vec(&ContainerHeader {
            vocabulary_id: self.vocabulary_id.clone(),
            vocabulary_version: self.vocabulary_version.clone(),
            vocabulary_digest: self.vocabulary_digest.clone(),
            modality: self.modality,
            adapter: self.adapter.clone(),
            count: self.ids.len() as u64,
            id_bytes,
        })?;
        if header.len() > MAX_HEADER {
            return Err(KuteError::LimitExceeded("token sequence header exceeds 64 KiB".into()));
        }
        let mut out = Vec::with_capacity(magic.len() + 4 + header.len() + body_hint);
        out.extend_from_slice(magic);
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        Ok(out)
    }

    /// Serialize to the compressed `.kute` container (format 2): the same
    /// header, then the ids as fixed-width little-endian integers inside one
    /// zlib stream. Smaller than format 1 on anything repetitive; `from_bytes`
    /// reads both.
    pub fn to_bytes_compressed(&self) -> Result<Vec<u8>> {
        let max = self.ids.iter().copied().max().unwrap_or(0);
        let width: usize = match max {
            0..=0xff => 1,
            0x100..=0xffff => 2,
            0x1_0000..=0xff_ffff => 3,
            _ => 4,
        };
        let mut plain = Vec::with_capacity(self.ids.len() * width);
        for &id in &self.ids {
            plain.extend_from_slice(&id.to_le_bytes()[..width]);
        }
        let out = self.start_container(MAGIC_COMPRESSED, Some(width as u8), plain.len() / 2)?;
        let mut encoder = flate2::write::ZlibEncoder::new(out, flate2::Compression::best());
        encoder.write_all(&plain)?;
        Ok(encoder.finish()?)
    }

    /// Parse a `.kute` container of either format. Rejects truncated,
    /// oversized or trailing data.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| KuteError::InvalidInput(format!("not a valid .kute token sequence: {why}"));
        if bytes.len() < MAGIC.len() + 4 {
            return Err(bad("missing KUTE magic"));
        }
        let compressed = match &bytes[..MAGIC.len()] {
            m if m == MAGIC => false,
            m if m == MAGIC_COMPRESSED => true,
            _ => return Err(bad("missing KUTE magic")),
        };
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
        let ids = if compressed {
            let width = match header.id_bytes {
                Some(w @ 1..=4) => w as usize,
                _ => return Err(bad("compressed container needs id_bytes between 1 and 4")),
            };
            let expected = header.count.checked_mul(width as u64).ok_or_else(|| bad("token count overflows"))?;
            if expected > (remaining as u64).saturating_mul(MAX_INFLATE_RATIO) {
                return Err(bad("declared token count exceeds what the data can hold"));
            }
            let mut decoder = flate2::read::ZlibDecoder::new(&bytes[pos..]);
            // The count comes from the file, so it only sizes the buffer up to a
            // fixed bound. Past that the buffer grows as real data arrives; a
            // header that claims gigabytes over a short or corrupt stream costs
            // nothing.
            let mut plain = Vec::with_capacity(expected.min(MAX_PREALLOCATE) as usize);
            // One byte past the expected length, so a longer stream is caught
            // and the stream checksum is verified.
            (&mut decoder).take(expected + 1).read_to_end(&mut plain).map_err(|e| bad(&format!("compressed ids: {e}")))?;
            if plain.len() as u64 != expected {
                return Err(bad("token count does not match the header"));
            }
            if decoder.total_in() != remaining as u64 {
                return Err(bad("data after the compressed ids"));
            }
            plain
                .chunks_exact(width)
                .map(|c| {
                    let mut le = [0u8; 4];
                    le[..width].copy_from_slice(c);
                    u32::from_le_bytes(le)
                })
                .collect()
        } else {
            if header.id_bytes.is_some() {
                return Err(bad("id_bytes belongs to the compressed format only"));
            }
            Self::read_varints(&bytes[pos..], header.count).map_err(bad)?
        };
        Ok(Self {
            vocabulary_id: header.vocabulary_id,
            vocabulary_version: header.vocabulary_version,
            vocabulary_digest: header.vocabulary_digest,
            modality: header.modality,
            adapter: header.adapter,
            ids,
        })
    }

    fn read_varints(bytes: &[u8], count: u64) -> std::result::Result<Vec<u32>, &'static str> {
        let bad = |why: &'static str| why;
        let mut pos = 0;
        if count > bytes.len() as u64 {
            return Err(bad("declared token count exceeds the data present"));
        }
        let mut ids = Vec::with_capacity(count as usize);
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
        if ids.len() as u64 != count {
            return Err(bad("token count does not match the header"));
        }
        Ok(ids)
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
    fn compressed_container_round_trip() {
        for ids in [vec![], vec![7], vec![0, 255], vec![0, 256, 65_535], vec![65_536, 3], vec![1, u32::MAX], (0..5000).map(|i| i % 97).collect()] {
            let s = seq(ids);
            let bytes = s.to_bytes_compressed().unwrap();
            assert_eq!(TokenSequence::from_bytes(&bytes).unwrap(), s);
        }
        let repetitive = seq((0..20_000).map(|i| 300 + i % 50).collect());
        assert!(repetitive.to_bytes_compressed().unwrap().len() * 10 < repetitive.to_bytes().unwrap().len());
    }

    #[test]
    fn compressed_container_rejects_damage() {
        let s = seq((0..400).map(|i| 1000 + i % 13).collect());
        let bytes = s.to_bytes_compressed().unwrap();
        for cut in 0..bytes.len() {
            assert!(TokenSequence::from_bytes(&bytes[..cut]).is_err(), "truncated at {cut}");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(TokenSequence::from_bytes(&extra).is_err());
        // A header that claims more ids than the body holds, and one that
        // claims an absurd number, are both refused.
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(text.contains("\"count\":400"));
        for claim in ["401", "399", "999999999999"] {
            let mut forged = Vec::new();
            let header_len = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) as usize;
            let header = String::from_utf8(bytes[9..9 + header_len].to_vec()).unwrap().replace("\"count\":400", &format!("\"count\":{claim}"));
            forged.extend_from_slice(&bytes[..5]);
            forged.extend_from_slice(&(header.len() as u32).to_le_bytes());
            forged.extend_from_slice(header.as_bytes());
            forged.extend_from_slice(&bytes[9 + header_len..]);
            assert!(TokenSequence::from_bytes(&forged).is_err(), "count {claim}");
        }
        // A header may claim as many ids as the ratio limit allows over a body
        // that is not a zlib stream at all. The reader must refuse it without
        // first reserving the claimed size (64 GiB here).
        let junk = 64usize << 20;
        let header = format!(
            "{{\"vocabulary_id\":\"kute:test\",\"vocabulary_version\":\"1.0.0\",\"vocabulary_digest\":\"sha256:00\",\"modality\":\"text\",\"count\":{},\"id_bytes\":1}}",
            junk as u64 * 1024
        );
        let mut bomb = Vec::with_capacity(9 + header.len() + junk);
        bomb.extend_from_slice(MAGIC_COMPRESSED);
        bomb.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bomb.extend_from_slice(header.as_bytes());
        bomb.resize(bomb.len() + junk, 0xaa);
        assert!(TokenSequence::from_bytes(&bomb).is_err());
        let mut flipped = bytes.clone();
        let last = flipped.len() - 6;
        flipped[last] ^= 0x40;
        assert!(TokenSequence::from_bytes(&flipped).is_err());
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
