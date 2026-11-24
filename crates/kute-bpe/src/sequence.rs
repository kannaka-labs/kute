//! BPE over sequences of named symbols instead of bytes: events, opcodes,
//! gates. The base alphabet is whatever symbols the corpus contains; merges
//! become composite tokens such as `[MOTOR_START, CURRENT_HIGH, MOTOR_TRIP]`.
//!
//! This is KUTE's own capability, not a rustbpe feature. A symbol that is not
//! in the alphabet is refused rather than approximated, because there is no
//! lossless way to represent it.

use crate::merge::{apply_merges, train_merges, Pair};
use kute_core::{check_sequence, sequence_for, CorpusFingerprint, CorpusSource, Tokenizer};
use kute_types::{
    CanonicalInput, KuteError, Manifest, MergeRule, Modality, Payload, PayloadKind, Provenance, Result,
    TestVector, TokenEntry, TokenKind, TokenPayload, TokenSequence, TokenizerCapabilities, TrainingConfig,
    Vocabulary, KUTE_VERSION, KVF_VERSION,
};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};

pub const ALGORITHM: &str = "sequence-bpe";

struct Loaded {
    vocabulary: Vocabulary,
    alphabet: FxHashMap<String, u32>,
    ranks: FxHashMap<Pair, u32>,
}

#[derive(Default)]
pub struct SequenceBpeTokenizer {
    loaded: Option<Loaded>,
}

impl SequenceBpeTokenizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn boxed() -> Box<dyn Tokenizer> {
        Box::new(Self::new())
    }

    fn loaded(&self) -> Result<&Loaded> {
        self.loaded.as_ref().ok_or_else(|| KuteError::NotTrained("train or load a vocabulary first".into()))
    }
}

fn encode_symbols(loaded: &Loaded, symbols: &[String]) -> Result<Vec<u32>> {
    let mut ids = Vec::with_capacity(symbols.len());
    for s in symbols {
        ids.push(*loaded.alphabet.get(s).ok_or_else(|| KuteError::UnknownSymbol(s.clone()))?);
    }
    apply_merges(&mut ids, &loaded.ranks);
    Ok(ids)
}

impl Tokenizer for SequenceBpeTokenizer {
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary> {
        config.validate()?;
        let mut fingerprint = CorpusFingerprint::new();
        let mut records: Vec<Vec<String>> = Vec::new();
        let mut modality = Modality::Events;
        while let Some(record) = corpus.next_record()? {
            fingerprint.update(&record);
            if let Some(limit) = config.max_corpus_units {
                if fingerprint.units() > limit {
                    return Err(KuteError::LimitExceeded(format!("corpus is larger than the max_corpus_units limit of {limit}")));
                }
            }
            modality = record.modality;
            match record.payload {
                Payload::Symbols(s) => records.push(s),
                Payload::Bytes(_) => {
                    return Err(KuteError::InvalidInput("sequence-bpe takes symbol payloads; use bpe for bytes".into()))
                }
            }
        }
        let alphabet_set: BTreeSet<&str> = records.iter().flatten().map(String::as_str).collect();
        if alphabet_set.is_empty() {
            return Err(KuteError::InvalidInput("the corpus contains no symbols".into()));
        }
        for s in &alphabet_set {
            if s.is_empty() || s.len() > kute_types::vocab::MAX_SYMBOL_BYTES {
                return Err(KuteError::InvalidInput("symbols must be 1 to 1024 bytes".into()));
            }
        }
        let base = alphabet_set.len() as u32;
        let reserved = config.reserved_tokens.len() as u32;
        if config.vocab_size < base + reserved {
            return Err(KuteError::InvalidConfig(format!(
                "the corpus has {base} distinct symbols; vocab_size must be at least {}",
                base + reserved
            )));
        }
        let index: FxHashMap<&str, u32> = alphabet_set.iter().enumerate().map(|(i, s)| (*s, i as u32)).collect();

        let mut window_counts: FxHashMap<Vec<u32>, u64> = FxHashMap::default();
        for record in &records {
            let ids: Vec<u32> = record.iter().map(|s| index[s.as_str()]).collect();
            for window in ids.chunks(config.train_window) {
                *window_counts.entry(window.to_vec()).or_insert(0) += 1;
            }
        }
        let mut windows: Vec<(Vec<u32>, u64)> = window_counts.into_iter().collect();
        windows.sort_unstable();
        let counts: Vec<u64> = windows.iter().map(|(_, c)| *c).collect();
        let words: Vec<Vec<u32>> = windows.into_iter().map(|(w, _)| w).collect();
        let unique_windows = words.len() as u64;

        let mut lengths: Vec<usize> = vec![1; base as usize];
        let limit = config.max_token_length;
        let learned = train_merges(words, &counts, base, config.vocab_size - base - reserved, config.min_frequency, |l, r| {
            let len = lengths[l as usize] + lengths[r as usize];
            if limit.is_none_or(|m| len <= m) && len <= kute_types::vocab::MAX_TOKEN_SYMBOLS {
                lengths.push(len);
                true
            } else {
                false
            }
        });

        let mut tokens: Vec<TokenEntry> = alphabet_set
            .iter()
            .enumerate()
            .map(|(i, s)| TokenEntry { id: i as u32, kind: TokenKind::Symbol, payload: TokenPayload::Symbols(vec![s.to_string()]), frequency: None })
            .collect();
        let mut rules = Vec::with_capacity(learned.len());
        for m in &learned {
            let mut seq = match &tokens[m.left as usize].payload {
                TokenPayload::Symbols(s) => s.clone(),
                TokenPayload::Bytes(_) => unreachable!("sequence vocabularies hold symbols only"),
            };
            if let TokenPayload::Symbols(s) = &tokens[m.right as usize].payload {
                seq.extend(s.iter().cloned());
            }
            tokens.push(TokenEntry { id: m.id, kind: TokenKind::Merge, payload: TokenPayload::Symbols(seq), frequency: Some(m.count) });
            rules.push(MergeRule { left: m.left, right: m.right, id: m.id });
        }
        for name in &config.reserved_tokens {
            let id = tokens.len() as u32;
            tokens.push(TokenEntry { id, kind: TokenKind::Special, payload: TokenPayload::Symbols(vec![name.clone()]), frequency: None });
        }

        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        statistics.insert("corpus_symbols".to_string(), fingerprint.units());
        statistics.insert("alphabet_size".to_string(), base as u64);
        statistics.insert("unique_windows".to_string(), unique_windows);
        statistics.insert("merges_learned".to_string(), learned.len() as u64);
        statistics.insert("requested_vocab_size".to_string(), config.vocab_size as u64);

        let vocabulary = Vocabulary {
            manifest: Manifest {
                kvf_version: KVF_VERSION.to_string(),
                id: config.vocabulary_id.clone(),
                version: config.vocabulary_version.clone(),
                algorithm: ALGORITHM.to_string(),
                modality,
                payload_kind: PayloadKind::Symbols,
                lossless: true,
                description: config.description.clone(),
                created: None,
                generator: format!("kute {KUTE_VERSION}"),
            },
            pretokenizer: None,
            adapter: None,
            tokens,
            merges: rules,
            statistics,
            provenance: Provenance {
                corpus_fingerprints: vec![fingerprint.finish()],
                license: config.license.clone(),
                training: Some(serde_json::to_value(config)?),
            },
            test_vectors: Vec::new(),
            representations: Vec::new(),
            integrity: None,
        };
        self.load(vocabulary)?;
        let loaded = self.loaded.as_mut().ok_or_else(|| KuteError::NotTrained("load failed".into()))?;
        let mut vectors = Vec::new();
        for record in records.iter().filter(|r| !r.is_empty()).take(4) {
            let sample: Vec<String> = record.iter().take(32).cloned().collect();
            let ids = encode_symbols(loaded, &sample)?;
            vectors.push(TestVector { input: TokenPayload::Symbols(sample), ids });
        }
        loaded.vocabulary.test_vectors = vectors;
        loaded.vocabulary.seal()?;
        Ok(loaded.vocabulary.clone())
    }

    fn load(&mut self, mut vocabulary: Vocabulary) -> Result<()> {
        vocabulary.validate()?;
        if vocabulary.manifest.algorithm != ALGORITHM {
            return Err(KuteError::VocabularyMismatch(format!(
                "the sequence-bpe tokenizer cannot interpret a {:?} vocabulary",
                vocabulary.manifest.algorithm
            )));
        }
        let mut alphabet: FxHashMap<String, u32> = FxHashMap::default();
        for t in &vocabulary.tokens {
            if t.kind == TokenKind::Symbol {
                let TokenPayload::Symbols(s) = &t.payload else {
                    return Err(KuteError::InvalidVocabulary(format!("symbol token {} has a byte payload", t.id)));
                };
                if s.len() != 1 || alphabet.insert(s[0].clone(), t.id).is_some() {
                    return Err(KuteError::InvalidVocabulary(format!("symbol token {} is not a single, unique symbol", t.id)));
                }
            }
        }
        let mut ranks: FxHashMap<Pair, u32> = FxHashMap::default();
        for m in &vocabulary.merges {
            if ranks.insert((m.left, m.right), m.id).is_some() {
                return Err(KuteError::InvalidVocabulary(format!("pair ({}, {}) has two merge rules", m.left, m.right)));
            }
        }
        if vocabulary.integrity.is_none() {
            vocabulary.seal()?;
        }
        self.loaded = Some(Loaded { vocabulary, alphabet, ranks });
        Ok(())
    }

    fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence> {
        let loaded = self.loaded()?;
        let Payload::Symbols(symbols) = &input.payload else {
            return Err(KuteError::InvalidInput("sequence-bpe takes symbol payloads".into()));
        };
        sequence_for(&loaded.vocabulary, input.modality, encode_symbols(loaded, symbols)?)
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
        let loaded = self.loaded()?;
        check_sequence(&loaded.vocabulary, tokens)?;
        let mut out = Vec::with_capacity(tokens.ids.len() * 2);
        for &id in &tokens.ids {
            match &loaded.vocabulary.token(id)?.payload {
                TokenPayload::Symbols(s) => out.extend(s.iter().cloned()),
                TokenPayload::Bytes(_) => return Err(KuteError::InvalidVocabulary("byte token in a symbol vocabulary".into())),
            }
        }
        Ok(CanonicalInput { modality: tokens.modality, payload: Payload::Symbols(out) })
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
            accepts: vec![PayloadKind::Symbols],
            rejects_unknown: true,
            notes: "BPE over named symbols. Lossless for sequences drawn from the trained alphabet; any other symbol is refused.".to_string(),
        }
    }
}
