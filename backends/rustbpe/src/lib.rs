//! The rustbpe reference backend behind the generic tokenizer interface.
//!
//! rustbpe (https://github.com/karpathy/rustbpe) is a text BPE trainer with
//! Python bindings. Used as a Rust crate it still links CPython and its only
//! training entry point takes a Python iterator, so this backend starts an
//! embedded interpreter to hand the corpus over. Training and encoding are
//! rustbpe's own code; KUTE adds the vocabulary model, validation and the
//! lossless contract around it.
//!
//! Scope, stated because rustbpe is narrower than the native backend:
//! valid UTF-8 text only, and encoding goes through rustbpe's regex split,
//! which drops any text the pattern does not match. This backend therefore
//! verifies every encode by decoding it and refuses input it cannot round-trip.

use kute_bpe::byte::{build_vocabulary, sample_prefix};
use kute_core::{check_sequence, sequence_for, CorpusFingerprint, CorpusSource, Tokenizer};
use kute_types::{
    CanonicalInput, KuteError, Payload, PayloadKind, Result, TestVector, TokenKind, TokenPayload,
    TokenSequence, TokenizerCapabilities, TrainingConfig, Vocabulary,
};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::collections::BTreeMap;

pub const ALGORITHM: &str = "rustbpe";

struct Loaded {
    vocabulary: Vocabulary,
    inner: rustbpe::Tokenizer,
}

#[derive(Default)]
pub struct RustBpeTokenizer {
    loaded: Option<Loaded>,
}

fn backend(e: impl std::fmt::Display) -> KuteError {
    KuteError::Backend(format!("rustbpe: {e}"))
}

/// Run rustbpe's trainer over `texts`. With no texts and 256 tokens this only
/// compiles the pattern, which is the one way to set it from Rust.
fn run_trainer(texts: &[String], vocab_size: u32, pattern: Option<&str>) -> Result<rustbpe::Tokenizer> {
    let mut inner = rustbpe::Tokenizer::new();
    Python::attach(|py| -> PyResult<()> {
        let list = PyList::new(py, texts)?;
        inner.train_from_iterator(py, list.as_any(), vocab_size, 8192, pattern.map(str::to_string))
    })
    .map_err(backend)?;
    Ok(inner)
}

impl RustBpeTokenizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn boxed() -> Box<dyn Tokenizer> {
        Box::new(Self::new())
    }

    fn loaded(&self) -> Result<&Loaded> {
        self.loaded.as_ref().ok_or_else(|| KuteError::NotTrained("train or load a vocabulary first".into()))
    }

    fn encode_text(loaded: &Loaded, text: &str) -> Result<Vec<u32>> {
        let ids = loaded.inner.encode(text);
        let mut back = Vec::with_capacity(text.len());
        for &id in &ids {
            if let TokenPayload::Bytes(b) = &loaded.vocabulary.token(id)?.payload {
                back.extend_from_slice(b);
            }
        }
        if back != text.as_bytes() {
            return Err(KuteError::Unsupported(
                "rustbpe dropped part of this input (its split pattern did not cover it); use the native bpe algorithm".into(),
            ));
        }
        Ok(ids)
    }
}

impl Tokenizer for RustBpeTokenizer {
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary> {
        config.validate()?;
        let reserved = config.reserved_tokens.len() as u32;
        if config.vocab_size < 256 + reserved {
            return Err(KuteError::InvalidConfig("rustbpe needs vocab_size of at least 256 plus reserved tokens".into()));
        }
        if config.min_frequency != 1 || config.max_token_length.is_some() {
            return Err(KuteError::Unsupported("rustbpe has no min_frequency or max_token_length; use the native bpe algorithm".into()));
        }
        let Some(pattern) = config.pretokenizer.pattern() else {
            return Err(KuteError::Unsupported("rustbpe always splits with a regex; set a pretokenizer (gpt4 is its default)".into()));
        };
        let mut fingerprint = CorpusFingerprint::new();
        let mut texts = Vec::new();
        while let Some(record) = corpus.next_record()? {
            fingerprint.update(&record);
            let Payload::Bytes(bytes) = record.payload else {
                return Err(KuteError::InvalidInput("rustbpe takes text".into()));
            };
            texts.push(String::from_utf8(bytes).map_err(|_| KuteError::InvalidInput("rustbpe takes valid UTF-8 text only".into()))?);
        }
        let inner = run_trainer(&texts, config.vocab_size - reserved, Some(pattern))?;
        let mut merges: Vec<(u32, u32, u32, Option<u64>)> = inner.merges.iter().map(|(&(l, r), &id)| (l, r, id, None)).collect();
        merges.sort_unstable_by_key(|m| m.2);

        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        statistics.insert("corpus_bytes".to_string(), fingerprint.units());
        statistics.insert("merges_learned".to_string(), merges.len() as u64);
        statistics.insert("requested_vocab_size".to_string(), config.vocab_size as u64);
        let vocabulary = build_vocabulary(ALGORITHM, config, &merges, statistics, fingerprint.finish())?;
        self.load(vocabulary)?;
        let loaded = self.loaded.as_mut().ok_or_else(|| KuteError::NotTrained("load failed".into()))?;
        let mut vectors = Vec::new();
        for text in texts.iter().filter(|t| !t.is_empty()).take(4) {
            let sample = String::from_utf8(sample_prefix(text.as_bytes(), 96)).map_err(backend)?;
            let ids = Self::encode_text(loaded, &sample)?;
            vectors.push(TestVector { input: TokenPayload::Bytes(sample.into_bytes()), ids });
        }
        loaded.vocabulary.test_vectors = vectors;
        loaded.vocabulary.seal()?;
        Ok(loaded.vocabulary.clone())
    }

    fn load(&mut self, mut vocabulary: Vocabulary) -> Result<()> {
        vocabulary.validate()?;
        if !kute_bpe::byte::COMPATIBLE_ALGORITHMS.contains(&vocabulary.manifest.algorithm.as_str()) {
            return Err(KuteError::VocabularyMismatch(format!(
                "the rustbpe backend cannot interpret a {:?} vocabulary",
                vocabulary.manifest.algorithm
            )));
        }
        let Some(pattern) = vocabulary.pretokenizer.clone() else {
            return Err(KuteError::Unsupported("rustbpe needs a vocabulary with a pretokenizer pattern".into()));
        };
        for (i, t) in vocabulary.tokens.iter().take(256).enumerate() {
            if t.kind != TokenKind::Byte || t.payload != TokenPayload::Bytes(vec![i as u8]) {
                return Err(KuteError::InvalidVocabulary(format!("token {i} must be the byte {i}")));
            }
        }
        let mut inner = run_trainer(&[], 256, Some(&pattern))?;
        inner.merges = vocabulary.merges.iter().map(|m| ((m.left, m.right), m.id)).collect();
        if vocabulary.integrity.is_none() {
            vocabulary.seal()?;
        }
        self.loaded = Some(Loaded { vocabulary, inner });
        Ok(())
    }

    fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence> {
        let loaded = self.loaded()?;
        let Payload::Bytes(bytes) = &input.payload else {
            return Err(KuteError::InvalidInput("rustbpe takes text".into()));
        };
        let text = std::str::from_utf8(bytes).map_err(|_| KuteError::InvalidInput("rustbpe takes valid UTF-8 text only".into()))?;
        sequence_for(&loaded.vocabulary, input.modality, Self::encode_text(loaded, text)?)
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
        let loaded = self.loaded()?;
        check_sequence(&loaded.vocabulary, tokens)?;
        let mut out = Vec::with_capacity(tokens.ids.len() * 3);
        for &id in &tokens.ids {
            if let TokenPayload::Bytes(b) = &loaded.vocabulary.token(id)?.payload {
                out.extend_from_slice(b);
            }
        }
        Ok(CanonicalInput::bytes(tokens.modality, out))
    }

    fn vocabulary(&self) -> Option<&Vocabulary> {
        self.loaded.as_ref().map(|l| &l.vocabulary)
    }

    fn capabilities(&self) -> TokenizerCapabilities {
        TokenizerCapabilities {
            algorithm: ALGORITHM.to_string(),
            lossless: true,
            trainable: true,
            deterministic: true,
            accepts: vec![PayloadKind::Bytes],
            rejects_unknown: true,
            notes: "rustbpe reference backend. Valid UTF-8 text only; input its split pattern cannot cover is refused. Needs a Python runtime.".to_string(),
        }
    }
}

/// Add the `rustbpe` algorithm to a registry.
pub fn register(registry: &mut kute_core::Registry) {
    registry.register_algorithm(ALGORITHM, RustBpeTokenizer::boxed);
}
