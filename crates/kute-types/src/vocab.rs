//! The in-memory vocabulary model. KVF (see `specifications/kvf`) is the
//! serialization of this structure; `kute-vocab` does the file handling.

use crate::{sha256_tagged, KuteError, Modality, PayloadKind, Result, Version, VocabularyId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The KVF format version this build reads and writes.
pub const KVF_VERSION: &str = "0.1";

pub const MAX_TOKENS: usize = 4_000_000;
pub const MAX_TOKEN_BYTES: usize = 64 * 1024;
pub const MAX_TOKEN_SYMBOLS: usize = 4096;
pub const MAX_SYMBOL_BYTES: usize = 1024;
pub const MAX_TEST_VECTORS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub kvf_version: String,
    pub id: VocabularyId,
    pub version: Version,
    /// Name of the algorithm that interprets this vocabulary, e.g. `bpe`.
    pub algorithm: String,
    pub modality: Modality,
    pub payload_kind: PayloadKind,
    /// True when every input the tokenizer accepts decodes back exactly.
    pub lossless: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional creation time. Left out by default so training is reproducible
    /// byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Software that wrote the file, e.g. `kute 0.1.0`.
    pub generator: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    /// One of the 256 byte values.
    Byte,
    /// A single Unicode character.
    Char,
    /// A single named symbol from a symbol alphabet.
    Symbol,
    /// A fixed dictionary entry.
    Entry,
    /// A learned token: the concatenation of two earlier tokens.
    Merge,
    /// A reserved token. Never produced by encoding content.
    Special,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPayload {
    /// Raw bytes, base64 in JSON.
    Bytes(#[serde(with = "crate::b64")] Vec<u8>),
    /// An ordered sequence of symbol names.
    Symbols(Vec<String>),
}

impl TokenPayload {
    pub fn kind(&self) -> PayloadKind {
        match self {
            TokenPayload::Bytes(_) => PayloadKind::Bytes,
            TokenPayload::Symbols(_) => PayloadKind::Symbols,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            TokenPayload::Bytes(b) => b.len(),
            TokenPayload::Symbols(s) => s.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn concat(&self, other: &TokenPayload) -> Option<TokenPayload> {
        match (self, other) {
            (TokenPayload::Bytes(a), TokenPayload::Bytes(b)) => {
                let mut v = Vec::with_capacity(a.len() + b.len());
                v.extend_from_slice(a);
                v.extend_from_slice(b);
                Some(TokenPayload::Bytes(v))
            }
            (TokenPayload::Symbols(a), TokenPayload::Symbols(b)) => {
                let mut v = Vec::with_capacity(a.len() + b.len());
                v.extend(a.iter().cloned());
                v.extend(b.iter().cloned());
                Some(TokenPayload::Symbols(v))
            }
            _ => None,
        }
    }

    /// Stable content hash of the payload, independent of any token id.
    pub fn content_hash(&self) -> String {
        match self {
            TokenPayload::Bytes(b) => {
                let mut buf = Vec::with_capacity(b.len() + 1);
                buf.push(b'b');
                buf.extend_from_slice(b);
                sha256_tagged(&buf)
            }
            TokenPayload::Symbols(s) => {
                let mut buf = vec![b's'];
                for sym in s {
                    buf.extend_from_slice(&(sym.len() as u64).to_le_bytes());
                    buf.extend_from_slice(sym.as_bytes());
                }
                sha256_tagged(&buf)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenEntry {
    pub id: u32,
    pub kind: TokenKind,
    #[serde(flatten)]
    pub payload: TokenPayload,
    /// Occurrences observed during training, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<u64>,
}

/// A learned transformation: tokens `left` and `right`, adjacent, become `id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MergeRule {
    pub left: u32,
    pub right: u32,
    pub id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
    /// Whether the adapter reconstructs the original bytes exactly.
    pub lossless: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// `sha256:` fingerprints of the training corpus as the tokenizer saw it.
    #[serde(default)]
    pub corpus_fingerprints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// The training configuration, kept so a run can be repeated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub training: Option<serde_json::Value>,
}

/// A known input and the ids it must encode to. Shipped inside the vocabulary
/// so any implementation can check itself against the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestVector {
    #[serde(flatten)]
    pub input: TokenPayload,
    pub ids: Vec<u32>,
}

/// Pointer to an independently versioned mapping from token ids to another
/// representation (FR-015). KVF only references mappings; it never embeds them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepresentationRef {
    pub mapping_id: String,
    pub mapping_version: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub key_id: String,
    pub algorithm: String,
    pub signature: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integrity {
    pub algorithm: String,
    pub digest: String,
    /// Reserved. KVF 0.1 defines the field but no signature scheme.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Signature>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vocabulary {
    pub manifest: Manifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pretokenizer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterSpec>,
    pub tokens: Vec<TokenEntry>,
    #[serde(default)]
    pub merges: Vec<MergeRule>,
    #[serde(default)]
    pub statistics: BTreeMap<String, u64>,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub test_vectors: Vec<TestVector>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub representations: Vec<RepresentationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Integrity>,
}

/// The token envelope from FR-005: one token with everything needed to
/// interpret it outside its vocabulary file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenRecord {
    pub token_id: u32,
    pub vocabulary_id: VocabularyId,
    pub vocabulary_version: Version,
    pub content_hash: String,
    pub symbol_type: TokenKind,
    pub modality: Modality,
    pub payload_encoding: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<u64>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Vocabulary {
    pub fn id(&self) -> &VocabularyId {
        &self.manifest.id
    }

    pub fn version(&self) -> &Version {
        &self.manifest.version
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn token(&self, id: u32) -> Result<&TokenEntry> {
        self.tokens.get(id as usize).ok_or(KuteError::UnknownToken(id))
    }

    /// SHA-256 over the canonical JSON of everything except `integrity`.
    pub fn compute_digest(&self) -> Result<String> {
        let mut body = self.clone();
        body.integrity = None;
        Ok(sha256_tagged(&serde_json::to_vec(&body)?))
    }

    /// The digest recorded in the file, or a freshly computed one.
    pub fn digest(&self) -> Result<String> {
        match &self.integrity {
            Some(i) => Ok(i.digest.clone()),
            None => self.compute_digest(),
        }
    }

    /// Record the content digest. Call after the last modification.
    pub fn seal(&mut self) -> Result<()> {
        let digest = self.compute_digest()?;
        self.integrity = Some(Integrity { algorithm: "sha256".into(), digest, signatures: Vec::new() });
        Ok(())
    }

    /// The FR-005 envelope for one token.
    pub fn record(&self, id: u32) -> Result<TokenRecord> {
        let t = self.token(id)?;
        let (encoding, payload) = match &t.payload {
            TokenPayload::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) => ("utf8", serde_json::Value::String(s.to_string())),
                Err(_) => ("base64", serde_json::Value::String(crate::b64::encode(b))),
            },
            TokenPayload::Symbols(s) => ("json", serde_json::json!(s)),
        };
        let mut metadata = BTreeMap::new();
        metadata.insert("reversible".to_string(), serde_json::json!(self.manifest.lossless));
        metadata.insert("algorithm".to_string(), serde_json::json!(self.manifest.algorithm));
        metadata.insert("length".to_string(), serde_json::json!(t.payload.len()));
        if let Some(rule) = self.merges.iter().find(|m| m.id == id) {
            metadata.insert("merge".to_string(), serde_json::json!([rule.left, rule.right]));
        }
        Ok(TokenRecord {
            token_id: id,
            vocabulary_id: self.manifest.id.clone(),
            vocabulary_version: self.manifest.version.clone(),
            content_hash: t.payload.content_hash(),
            symbol_type: t.kind,
            modality: self.manifest.modality,
            payload_encoding: encoding.to_string(),
            payload,
            frequency: t.frequency,
            metadata,
        })
    }

    /// Structural validation. Everything that loads a vocabulary from outside
    /// the process must call this before using it.
    pub fn validate(&self) -> Result<()> {
        let bad = |why: String| Err(KuteError::InvalidVocabulary(why));
        if self.manifest.kvf_version != KVF_VERSION {
            return Err(KuteError::Unsupported(format!(
                "KVF version {:?}; this build reads {KVF_VERSION:?}",
                self.manifest.kvf_version
            )));
        }
        if self.manifest.algorithm.is_empty() || self.manifest.algorithm.len() > 64 {
            return bad("algorithm name must be 1 to 64 bytes".into());
        }
        if self.tokens.is_empty() {
            return bad("no tokens".into());
        }
        if self.tokens.len() > MAX_TOKENS {
            return bad(format!("more than {MAX_TOKENS} tokens"));
        }
        if let Some(p) = &self.pretokenizer {
            if p.is_empty() || p.len() > 4096 {
                return bad("pretokenizer pattern must be 1 to 4096 bytes".into());
            }
        }
        for (i, t) in self.tokens.iter().enumerate() {
            if t.id as usize != i {
                return bad(format!("token at position {i} has id {}; ids must be dense and ordered", t.id));
            }
            if t.payload.kind() != self.manifest.payload_kind && t.kind != TokenKind::Special {
                return bad(format!("token {i} payload kind differs from the manifest"));
            }
            match &t.payload {
                TokenPayload::Bytes(b) => {
                    if b.is_empty() || b.len() > MAX_TOKEN_BYTES {
                        return bad(format!("token {i} payload must be 1 to {MAX_TOKEN_BYTES} bytes"));
                    }
                }
                TokenPayload::Symbols(s) => {
                    if s.is_empty() || s.len() > MAX_TOKEN_SYMBOLS {
                        return bad(format!("token {i} must hold 1 to {MAX_TOKEN_SYMBOLS} symbols"));
                    }
                    if s.iter().any(|x| x.is_empty() || x.len() > MAX_SYMBOL_BYTES) {
                        return bad(format!("token {i} has an empty or oversized symbol"));
                    }
                }
            }
        }
        let mut has_rule = vec![false; self.tokens.len()];
        for m in &self.merges {
            let n = self.tokens.len() as u32;
            if m.id >= n || m.left >= m.id || m.right >= m.id {
                return bad(format!("merge rule ({}, {}) -> {} refers to a token that is not earlier in the table", m.left, m.right, m.id));
            }
            let target = &self.tokens[m.id as usize];
            if target.kind != TokenKind::Merge {
                return bad(format!("merge rule targets token {} which is not a merge token", m.id));
            }
            if has_rule[m.id as usize] {
                return bad(format!("token {} has more than one merge rule", m.id));
            }
            has_rule[m.id as usize] = true;
            let expected = self.tokens[m.left as usize].payload.concat(&self.tokens[m.right as usize].payload);
            if expected.as_ref() != Some(&target.payload) {
                return bad(format!("token {} payload is not the concatenation of tokens {} and {}", m.id, m.left, m.right));
            }
        }
        for (i, t) in self.tokens.iter().enumerate() {
            if t.kind == TokenKind::Merge && !has_rule[i] {
                return bad(format!("merge token {i} has no merge rule"));
            }
        }
        if self.test_vectors.len() > MAX_TEST_VECTORS {
            return bad(format!("more than {MAX_TEST_VECTORS} test vectors"));
        }
        for (i, v) in self.test_vectors.iter().enumerate() {
            if v.ids.iter().any(|&id| id as usize >= self.tokens.len()) {
                return bad(format!("test vector {i} uses a token id outside the table"));
            }
        }
        if let Some(integrity) = &self.integrity {
            if integrity.algorithm != "sha256" {
                return Err(KuteError::Unsupported(format!("integrity algorithm {:?}", integrity.algorithm)));
            }
            let actual = self.compute_digest()?;
            if actual != integrity.digest {
                return Err(KuteError::Integrity(format!(
                    "recorded digest {} does not match content digest {actual}",
                    integrity.digest
                )));
            }
        }
        Ok(())
    }
}
