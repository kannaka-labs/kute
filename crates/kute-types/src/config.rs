use crate::{KuteError, PayloadKind, Result, Version, VocabularyId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The GPT-4 style split pattern, the same default rustbpe and tiktoken use.
pub const GPT4_PATTERN: &str = r"'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]++[\r\n]*|\s*[\r\n]|\s+(?!\S)|\s+";

/// How byte input is split into chunks before merges are learned or applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Pretokenizer {
    /// No splitting. Right for binary data.
    #[default]
    None,
    /// The GPT-4 split pattern.
    Gpt4,
    /// A caller-supplied regular expression.
    Regex(String),
}

impl Pretokenizer {
    pub fn pattern(&self) -> Option<&str> {
        match self {
            Pretokenizer::None => None,
            Pretokenizer::Gpt4 => Some(GPT4_PATTERN),
            Pretokenizer::Regex(p) => Some(p.as_str()),
        }
    }
}

/// Everything a training run needs. Identical configuration and corpus must
/// produce an identical vocabulary (FR-003 deterministic mode is the only mode
/// in v0.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    pub vocabulary_id: VocabularyId,
    pub vocabulary_version: Version,
    /// Target total number of tokens, including base and reserved tokens.
    pub vocab_size: u32,
    /// A candidate must occur at least this often to become a token.
    #[serde(default = "one")]
    pub min_frequency: u64,
    #[serde(default)]
    pub pretokenizer: Pretokenizer,
    /// Special tokens appended after learned tokens. Never produced by encode.
    #[serde(default)]
    pub reserved_tokens: Vec<String>,
    /// Upper bound on a learned token's length in units (bytes or symbols).
    #[serde(default)]
    pub max_token_length: Option<usize>,
    /// Window length used to split unsegmented input while counting.
    #[serde(default = "default_window")]
    pub train_window: usize,
    /// Fixed entries for the dictionary algorithm.
    #[serde(default)]
    pub dictionary: Vec<String>,
    /// Job limit: refuse a corpus larger than this many units.
    #[serde(default)]
    pub max_corpus_units: Option<u64>,
    /// Recorded for provenance. No v0.1 algorithm consumes randomness.
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// Store short samples of the training corpus in the vocabulary as
    /// self-test vectors. Off unless asked for: the vectors are verbatim
    /// excerpts of the corpus, and a vocabulary file is what gets published.
    /// Absent means off, for a configuration someone writes by hand as much
    /// as for one built in code. Vocabularies trained before v0.4.0 recorded
    /// nothing here when vectors were on; whether a vocabulary has vectors
    /// is read from the vocabulary, not from this field.
    #[serde(default)]
    pub embed_test_vectors: bool,
    /// When training from files, read them in pieces of about this many
    /// bytes, cut at a line break, instead of whole. Each piece is one
    /// record, so memory no longer grows with file size. The value is part
    /// of the configuration because it decides where records end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_bytes: Option<usize>,
    /// Continue an earlier run instead of starting over. The vocabulary
    /// given here must have been trained on the same corpus with the same
    /// settings and a smaller `vocab_size`; its merges are kept and learning
    /// goes on from where it stopped. The result is the vocabulary a single
    /// run to the new size would have produced. Not part of the recorded
    /// configuration: it changes how the result is reached, not the result.
    /// Its merges are taken as given, so resume only from a vocabulary you
    /// trained or trust.
    #[serde(skip)]
    pub resume_from: Option<Box<crate::Vocabulary>>,
}

fn one() -> u64 {
    1
}

fn default_window() -> usize {
    256
}

impl TrainingConfig {
    pub fn new(vocabulary_id: VocabularyId, vocab_size: u32) -> Self {
        Self {
            vocabulary_id,
            vocabulary_version: Version::new(0, 1, 0),
            vocab_size,
            min_frequency: 1,
            pretokenizer: Pretokenizer::None,
            reserved_tokens: Vec::new(),
            max_token_length: None,
            train_window: default_window(),
            dictionary: Vec::new(),
            max_corpus_units: None,
            seed: 0,
            description: None,
            license: None,
            embed_test_vectors: false,
            stream_bytes: None,
            resume_from: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        const MAX_VOCAB: u32 = 4_000_000;
        if self.vocab_size == 0 || self.vocab_size > MAX_VOCAB {
            return Err(KuteError::InvalidConfig(format!("vocab_size must be between 1 and {MAX_VOCAB}")));
        }
        if self.min_frequency == 0 {
            return Err(KuteError::InvalidConfig("min_frequency must be at least 1".into()));
        }
        if self.train_window < 2 || self.train_window > 1 << 20 {
            return Err(KuteError::InvalidConfig("train_window must be between 2 and 1048576".into()));
        }
        if self.stream_bytes.is_some_and(|n| !(4096..=1 << 30).contains(&n)) {
            return Err(KuteError::InvalidConfig("stream_bytes must be between 4096 and 1073741824".into()));
        }
        if self.max_token_length == Some(0) {
            return Err(KuteError::InvalidConfig("max_token_length must be at least 1".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &self.reserved_tokens {
            if t.is_empty() || t.len() > 256 {
                return Err(KuteError::InvalidConfig("reserved tokens must be 1 to 256 bytes".into()));
            }
            if !seen.insert(t) {
                return Err(KuteError::InvalidConfig(format!("reserved token {t:?} is listed twice")));
            }
        }
        if let Pretokenizer::Regex(p) = &self.pretokenizer {
            if p.is_empty() || p.len() > 4096 {
                return Err(KuteError::InvalidConfig("pretokenizer regex must be 1 to 4096 bytes".into()));
            }
        }
        Ok(())
    }
}

/// Free-form adapter options, e.g. `{"field": "event"}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterConfig {
    #[serde(flatten)]
    pub options: BTreeMap<String, String>,
}

impl AdapterConfig {
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.options.insert(key.to_string(), value.to_string());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(String::as_str)
    }
}

/// What a tokenizer declares about itself (FR-002, ADR-0007).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenizerCapabilities {
    pub algorithm: String,
    /// `decode(encode(x)) == x` for every input the tokenizer accepts.
    pub lossless: bool,
    pub trainable: bool,
    pub deterministic: bool,
    pub accepts: Vec<PayloadKind>,
    /// Inputs outside the vocabulary are refused instead of approximated.
    pub rejects_unknown: bool,
    pub notes: String,
}
