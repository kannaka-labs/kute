//! Shared data model for the Kannaka Universal Tokenization Engine.
//!
//! This crate has no tokenization logic. It defines the values every other
//! crate exchanges: identifiers, canonical inputs, token sequences, training
//! configuration and the in-memory vocabulary model that KVF serializes.

pub mod config;
pub mod error;
pub mod id;
pub mod input;
pub mod vocab;

pub use config::{AdapterConfig, Pretokenizer, TokenizerCapabilities, TrainingConfig};
pub use error::{KuteError, Result};
pub use id::VocabularyId;
pub use input::{CanonicalInput, Modality, Payload, PayloadKind, TokenSequence, UnknownPolicy, MAX_ESCAPES};
pub use semver::Version;
pub use vocab::{
    AdapterSpec, Integrity, Manifest, MergeRule, Provenance, RepresentationRef, Signature,
    TestVector, TokenEntry, TokenKind, TokenPayload, TokenRecord, Vocabulary, KVF_VERSION,
};

/// Version of the KUTE software that produced an artifact.
pub const KUTE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hex SHA-256 with the `sha256:` prefix used throughout KUTE.
pub fn sha256_tagged(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Serde helper: `Vec<u8>` as standard base64 text.
pub mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }

    pub fn encode(v: &[u8]) -> String {
        STANDARD.encode(v)
    }
}
