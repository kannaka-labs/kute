use thiserror::Error;

/// Every failure KUTE reports. Library code returns these instead of panicking,
/// including on malformed or hostile input.
#[derive(Debug, Error)]
pub enum KuteError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid vocabulary: {0}")]
    InvalidVocabulary(String),
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("unknown token id {0}")]
    UnknownToken(u32),
    #[error("unknown symbol {0:?}: it is not in the vocabulary and this vocabulary has no lossless fallback for it")]
    UnknownSymbol(String),
    #[error("vocabulary mismatch: {0}")]
    VocabularyMismatch(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("tokenizer has no vocabulary: {0}")]
    NotTrained(String),
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("backend error: {0}")]
    Backend(String),
}

impl From<serde_json::Error> for KuteError {
    fn from(e: serde_json::Error) -> Self {
        KuteError::Serde(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, KuteError>;
