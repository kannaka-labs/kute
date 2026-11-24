//! BPE over sequences of named symbols instead of bytes: events, opcodes,
//! gates. The base alphabet is whatever symbols the corpus contains; merges
//! become composite tokens such as `[MOTOR_START, CURRENT_HIGH, MOTOR_TRIP]`.
//!
//! This is KUTE's own capability, not a rustbpe feature.
//!
//! A vocabulary is frozen, but the world is not: new event names appear. A
//! symbol outside the alphabet is never approximated. Under the strict
//! policy it is refused; under the escape policy it travels literally in
//! the sequence's escape table, the vocabulary stays as it was, and decoding
//! returns the input exactly. Merges never span an escaped symbol.

use crate::merge::{apply_merges, train_merges, Pair};
use kute_core::{check_sequence, sequence_for, CorpusFingerprint, CorpusSource, Tokenizer};
use kute_types::{
    CanonicalInput, KuteError, Manifest, MergeRule, Modality, Payload, PayloadKind, Provenance, Result,
    TestVector, TokenEntry, TokenKind, TokenPayload, TokenSequence, TokenizerCapabilities, TrainingConfig,
    UnknownPolicy, Vocabulary, KUTE_VERSION, KVF_VERSION, MAX_ESCAPES,
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

/// Encode with unknown symbols escaped. Returns the ids and the escape
/// table; an escaped symbol gets the id `table length + its index`, and the
/// same symbol always reuses one entry.
fn encode_symbols_open(loaded: &Loaded, symbols: &[String]) -> Result<(Vec<u32>, Vec<String>)> {
    let table = loaded.vocabulary.tokens.len() as u32;
    let mut out = Vec::with_capacity(symbols.len());
    let mut run: Vec<u32> = Vec::new();
    let mut escapes: Vec<String> = Vec::new();
    let mut escape_index: FxHashMap<&str, u32> = FxHashMap::default();
    for s in symbols {
        match loaded.alphabet.get(s) {
            Some(&id) => run.push(id),
            None => {
                if s.is_empty() || s.len() > kute_types::vocab::MAX_SYMBOL_BYTES {
                    return Err(KuteError::InvalidInput("symbols must be 1 to 1024 bytes".into()));
                }
                apply_merges(&mut run, &loaded.ranks);
                out.append(&mut run);
                let index = match escape_index.get(s.as_str()) {
                    Some(&i) => i,
                    None => {
                        if escapes.len() >= MAX_ESCAPES {
                            return Err(KuteError::LimitExceeded(format!("more than {MAX_ESCAPES} distinct unknown symbols in one sequence")));
                        }
                        escapes.push(s.clone());
                        escape_index.insert(s.as_str(), escapes.len() as u32 - 1);
                        escapes.len() as u32 - 1
                    }
                };
                out.push(table + index);
            }
        }
    }
    apply_merges(&mut run, &loaded.ranks);
    out.append(&mut run);
    Ok((out, escapes))
}

impl Tokenizer for SequenceBpeTokenizer {
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary> {
        config.validate()?;
        // One pass. Only the window counts, the alphabet and a few sample
        // records are kept, so memory follows the number of distinct
        // windows, not the size of the corpus. Symbols get provisional ids
        // in order of first appearance and are renumbered in sorted order
        // afterwards, which keeps the result independent of that order.
        let mut fingerprint = CorpusFingerprint::new();
        let mut modality = Modality::Events;
        let mut seen: FxHashMap<String, u32> = FxHashMap::default();
        let mut provisional: FxHashMap<Vec<u32>, u64> = FxHashMap::default();
        let mut samples: Vec<Vec<String>> = Vec::new();
        let mut ids: Vec<u32> = Vec::new();
        while let Some(record) = corpus.next_record()? {
            fingerprint.update(&record);
            if let Some(limit) = config.max_corpus_units {
                if fingerprint.units() > limit {
                    return Err(KuteError::LimitExceeded(format!("corpus is larger than the max_corpus_units limit of {limit}")));
                }
            }
            modality = record.modality;
            let Payload::Symbols(symbols) = record.payload else {
                return Err(KuteError::InvalidInput("sequence-bpe takes symbol payloads; use bpe for bytes".into()));
            };
            if samples.len() < 4 && !symbols.is_empty() {
                samples.push(symbols.iter().take(32).cloned().collect());
            }
            ids.clear();
            for s in symbols {
                if s.is_empty() || s.len() > kute_types::vocab::MAX_SYMBOL_BYTES {
                    return Err(KuteError::InvalidInput("symbols must be 1 to 1024 bytes".into()));
                }
                let next = seen.len() as u32;
                ids.push(*seen.entry(s).or_insert(next));
            }
            for window in ids.chunks(config.train_window) {
                match provisional.get_mut(window) {
                    Some(count) => *count += 1,
                    None => {
                        provisional.insert(window.to_vec(), 1);
                    }
                }
            }
        }
        let alphabet_set: BTreeSet<&str> = seen.keys().map(String::as_str).collect();
        if alphabet_set.is_empty() {
            return Err(KuteError::InvalidInput("the corpus contains no symbols".into()));
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
        let mut renumber = vec![0u32; seen.len()];
        for (symbol, &first) in &seen {
            renumber[first as usize] = index[symbol.as_str()];
        }
        let mut windows: Vec<(Vec<u32>, u64)> =
            provisional.into_iter().map(|(w, c)| (w.into_iter().map(|id| renumber[id as usize]).collect(), c)).collect();
        windows.sort_unstable();
        let counts: Vec<u64> = windows.iter().map(|(_, c)| *c).collect();
        let words: Vec<Vec<u32>> = windows.into_iter().map(|(w, _)| w).collect();
        let unique_windows = words.len() as u64;

        let mut lengths: Vec<usize> = vec![1; base as usize];
        let limit = config.max_token_length;
        let learned = train_merges(words, &counts, base, config.vocab_size - base - reserved, config.min_frequency, |l, r| {
            let len = lengths[l as usize] + lengths[r as usize];
            if limit.map_or(true, |m| len <= m) && len <= kute_types::vocab::MAX_TOKEN_SYMBOLS {
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
        for sample in samples.into_iter().filter(|_| config.embed_test_vectors) {
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

    fn encode_open(&self, input: &CanonicalInput, policy: UnknownPolicy) -> Result<TokenSequence> {
        if policy == UnknownPolicy::Strict {
            return self.encode(input);
        }
        let loaded = self.loaded()?;
        let Payload::Symbols(symbols) = &input.payload else {
            return Err(KuteError::InvalidInput("sequence-bpe takes symbol payloads".into()));
        };
        let (ids, escapes) = encode_symbols_open(loaded, symbols)?;
        let mut sequence = sequence_for(&loaded.vocabulary, input.modality, ids)?;
        sequence.escapes = escapes;
        Ok(sequence)
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
        let loaded = self.loaded()?;
        check_sequence(&loaded.vocabulary, tokens)?;
        let mut out = Vec::with_capacity(tokens.ids.len() * 2);
        let table = loaded.vocabulary.tokens.len();
        for &id in &tokens.ids {
            if id as usize >= table {
                let escaped = tokens.escapes.get(id as usize - table).ok_or(KuteError::UnknownToken(id))?;
                out.push(escaped.clone());
                continue;
            }
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
            rejects_unknown: false,
            notes: "BPE over named symbols. Lossless for the trained alphabet. A symbol outside it is refused under the strict policy and carried literally, without changing the vocabulary, under the escape policy.".to_string(),
        }
    }
}

/// Most additions one evolution step accepts.
pub const MAX_ADDITIONS: usize = 65_536;

/// Derive the next version of a sequence vocabulary by adding symbols and
/// symbol sequences (development directive v0.2, vocabulary evolution).
///
/// The parent is not modified. Every token of the parent keeps its id and
/// payload, so the child's table extends the parent's. New single symbols
/// are appended as symbol tokens; a sequence is appended as a chain of merge
/// tokens, reusing any merge the parent already has. The result is a pure
/// function of the parent, `additions` and `version`: two parties that
/// apply the same step hold the same digest.
///
/// Encoding can differ between parent and child, since the child knows more
/// merges. A sequence encoded under the parent still names the parent's
/// digest; re-encode it to move it to the child.
pub fn evolve(parent: &Vocabulary, additions: &[Vec<String>], version: kute_types::Version) -> Result<Vocabulary> {
    parent.validate()?;
    if parent.manifest.algorithm != ALGORITHM {
        return Err(KuteError::Unsupported(format!("evolution is defined for {ALGORITHM} vocabularies, not {:?}", parent.manifest.algorithm)));
    }
    if version <= parent.manifest.version {
        return Err(KuteError::InvalidConfig(format!("the new version {version} must be greater than {}", parent.manifest.version)));
    }
    if additions.is_empty() || additions.len() > MAX_ADDITIONS {
        return Err(KuteError::InvalidConfig(format!("an evolution step takes 1 to {MAX_ADDITIONS} additions")));
    }
    let mut child = parent.clone();
    child.integrity = None;
    child.manifest.version = version;
    child.manifest.generator = format!("kute {KUTE_VERSION}");
    let mut by_symbol: FxHashMap<String, u32> = FxHashMap::default();
    let mut by_pair: FxHashMap<Pair, u32> = FxHashMap::default();
    for t in &child.tokens {
        if let (TokenKind::Symbol, TokenPayload::Symbols(s)) = (t.kind, &t.payload) {
            by_symbol.insert(s[0].clone(), t.id);
        }
    }
    for m in &child.merges {
        by_pair.insert((m.left, m.right), m.id);
    }
    // Reserved tokens are not symbols of the alphabet; a symbol of the same
    // name would be ambiguous to a reader.
    let reserved: BTreeSet<&str> = parent
        .tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Special)
        .filter_map(|t| match &t.payload {
            TokenPayload::Symbols(s) => Some(s[0].as_str()),
            TokenPayload::Bytes(_) => None,
        })
        .collect();
    let before = child.tokens.len();
    for addition in additions {
        if addition.is_empty() || addition.len() > kute_types::vocab::MAX_TOKEN_SYMBOLS {
            return Err(KuteError::InvalidInput(format!("an addition must hold 1 to {} symbols", kute_types::vocab::MAX_TOKEN_SYMBOLS)));
        }
        let mut ids = Vec::with_capacity(addition.len());
        for symbol in addition {
            if symbol.is_empty() || symbol.len() > kute_types::vocab::MAX_SYMBOL_BYTES {
                return Err(KuteError::InvalidInput("symbols must be 1 to 1024 bytes".into()));
            }
            if reserved.contains(symbol.as_str()) {
                return Err(KuteError::InvalidInput(format!("{symbol:?} is a reserved token of this vocabulary")));
            }
            let id = match by_symbol.get(symbol) {
                Some(&id) => id,
                None => {
                    let id = child.tokens.len() as u32;
                    child.tokens.push(TokenEntry { id, kind: TokenKind::Symbol, payload: TokenPayload::Symbols(vec![symbol.clone()]), frequency: None });
                    by_symbol.insert(symbol.clone(), id);
                    id
                }
            };
            ids.push(id);
        }
        let mut left = ids[0];
        let mut payload = vec![addition[0].clone()];
        for (i, &right) in ids.iter().enumerate().skip(1) {
            payload.push(addition[i].clone());
            left = match by_pair.get(&(left, right)) {
                Some(&id) => id,
                None => {
                    let id = child.tokens.len() as u32;
                    child.tokens.push(TokenEntry { id, kind: TokenKind::Merge, payload: TokenPayload::Symbols(payload.clone()), frequency: None });
                    child.merges.push(MergeRule { left, right, id });
                    by_pair.insert((left, right), id);
                    id
                }
            };
        }
    }
    let added = child.tokens.len() - before;
    if added == 0 {
        return Err(KuteError::InvalidInput("every addition is already in the vocabulary".into()));
    }
    if child.tokens.len() > kute_types::vocab::MAX_TOKENS {
        return Err(KuteError::LimitExceeded("the evolved vocabulary would exceed the token limit".into()));
    }
    child.statistics.insert("evolved_tokens".to_string(), added as u64);
    child.provenance.training = Some(serde_json::json!({
        "evolved_from": { "version": parent.manifest.version.to_string(), "digest": parent.digest()? },
        "additions": additions,
        "parent_training": parent.provenance.training,
    }));
    // Expected ids of the parent's test vectors may change under the new
    // merges; recompute them against the child.
    let inputs: Vec<TokenPayload> = std::mem::take(&mut child.test_vectors).into_iter().map(|v| v.input).collect();
    let mut tokenizer = SequenceBpeTokenizer::new();
    tokenizer.load(child)?;
    let loaded = tokenizer.loaded.as_mut().ok_or_else(|| KuteError::NotTrained("load failed".into()))?;
    let mut vectors = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let TokenPayload::Symbols(symbols) = &input {
            let ids = encode_symbols(loaded, symbols)?;
            vectors.push(TestVector { input, ids });
        }
    }
    loaded.vocabulary.test_vectors = vectors;
    loaded.vocabulary.seal()?;
    Ok(loaded.vocabulary.clone())
}
