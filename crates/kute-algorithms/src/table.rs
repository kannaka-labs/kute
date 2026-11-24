//! The three baseline tokenizers share one engine: a table of tokens matched
//! greedily, longest first, against the input.

use crate::{BYTES, CHAR, DICTIONARY};
use kute_core::{check_sequence, sequence_for, CorpusFingerprint, CorpusSource, Tokenizer};
use kute_types::{
    CanonicalInput, KuteError, Manifest, Modality, Payload, PayloadKind, Provenance, Result, TestVector,
    TokenEntry, TokenKind, TokenPayload, TokenSequence, TokenizerCapabilities, TrainingConfig, Vocabulary,
    KUTE_VERSION, KVF_VERSION,
};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};

/// A loaded table vocabulary and the indexes needed to match against it.
struct Table {
    vocabulary: Vocabulary,
    /// Byte entries longer than one byte, keyed by content.
    entries: FxHashMap<Vec<u8>, u32>,
    /// Lengths present in `entries`, longest first.
    lengths: Vec<usize>,
    symbols: FxHashMap<String, u32>,
}

impl Table {
    fn build(mut vocabulary: Vocabulary, algorithm: &str) -> Result<Self> {
        vocabulary.validate()?;
        if vocabulary.manifest.algorithm != algorithm {
            return Err(KuteError::VocabularyMismatch(format!(
                "the {algorithm} tokenizer cannot interpret a {:?} vocabulary",
                vocabulary.manifest.algorithm
            )));
        }
        if !vocabulary.merges.is_empty() {
            return Err(KuteError::InvalidVocabulary(format!("a {algorithm} vocabulary has no merge rules")));
        }
        let mut entries: FxHashMap<Vec<u8>, u32> = FxHashMap::default();
        let mut symbols: FxHashMap<String, u32> = FxHashMap::default();
        let mut lengths = BTreeSet::new();
        match vocabulary.manifest.payload_kind {
            PayloadKind::Bytes => {
                if vocabulary.tokens.len() < 256 {
                    return Err(KuteError::InvalidVocabulary("byte vocabularies start with the 256 byte tokens".into()));
                }
                for (i, t) in vocabulary.tokens.iter().enumerate() {
                    let TokenPayload::Bytes(b) = &t.payload else {
                        return Err(KuteError::InvalidVocabulary(format!("token {i} is not a byte token")));
                    };
                    if i < 256 {
                        if t.kind != TokenKind::Byte || b.as_slice() != [i as u8] {
                            return Err(KuteError::InvalidVocabulary(format!("token {i} must be the byte {i}")));
                        }
                    } else if t.kind != TokenKind::Special {
                        if b.len() < 2 {
                            return Err(KuteError::InvalidVocabulary(format!("token {i} duplicates a byte token")));
                        }
                        if entries.insert(b.clone(), t.id).is_some() {
                            return Err(KuteError::InvalidVocabulary(format!("token {i} duplicates an earlier entry")));
                        }
                        lengths.insert(b.len());
                    }
                }
            }
            PayloadKind::Symbols => {
                for (i, t) in vocabulary.tokens.iter().enumerate() {
                    if t.kind == TokenKind::Special {
                        continue;
                    }
                    let TokenPayload::Symbols(s) = &t.payload else {
                        return Err(KuteError::InvalidVocabulary(format!("token {i} is not a symbol token")));
                    };
                    if s.len() != 1 || symbols.insert(s[0].clone(), t.id).is_some() {
                        return Err(KuteError::InvalidVocabulary(format!("token {i} is not a single, unique symbol")));
                    }
                }
            }
        }
        if vocabulary.integrity.is_none() {
            vocabulary.seal()?;
        }
        Ok(Self { vocabulary, entries, lengths: lengths.into_iter().rev().collect(), symbols })
    }

    fn encode(&self, input: &CanonicalInput) -> Result<Vec<u32>> {
        match (&input.payload, self.vocabulary.manifest.payload_kind) {
            (Payload::Bytes(bytes), PayloadKind::Bytes) => {
                let mut ids = Vec::with_capacity(bytes.len());
                let mut i = 0;
                'outer: while i < bytes.len() {
                    for &len in &self.lengths {
                        if i + len <= bytes.len() {
                            if let Some(&id) = self.entries.get(&bytes[i..i + len]) {
                                ids.push(id);
                                i += len;
                                continue 'outer;
                            }
                        }
                    }
                    ids.push(bytes[i] as u32);
                    i += 1;
                }
                Ok(ids)
            }
            (Payload::Symbols(symbols), PayloadKind::Symbols) => symbols
                .iter()
                .map(|s| self.symbols.get(s).copied().ok_or_else(|| KuteError::UnknownSymbol(s.clone())))
                .collect(),
            (Payload::Bytes(_), PayloadKind::Symbols) => Err(KuteError::InvalidInput("this vocabulary takes symbol payloads".into())),
            (Payload::Symbols(_), PayloadKind::Bytes) => Err(KuteError::InvalidInput("this vocabulary takes byte payloads".into())),
        }
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
        check_sequence(&self.vocabulary, tokens)?;
        match self.vocabulary.manifest.payload_kind {
            PayloadKind::Bytes => {
                let mut out = Vec::with_capacity(tokens.ids.len());
                for &id in &tokens.ids {
                    if let TokenPayload::Bytes(b) = &self.vocabulary.token(id)?.payload {
                        out.extend_from_slice(b);
                    }
                }
                Ok(CanonicalInput::bytes(tokens.modality, out))
            }
            PayloadKind::Symbols => {
                let mut out = Vec::with_capacity(tokens.ids.len());
                for &id in &tokens.ids {
                    if let TokenPayload::Symbols(s) = &self.vocabulary.token(id)?.payload {
                        out.extend(s.iter().cloned());
                    }
                }
                Ok(CanonicalInput { modality: tokens.modality, payload: Payload::Symbols(out) })
            }
        }
    }
}

fn byte_tokens() -> Vec<TokenEntry> {
    (0u32..256)
        .map(|b| TokenEntry { id: b, kind: TokenKind::Byte, payload: TokenPayload::Bytes(vec![b as u8]), frequency: None })
        .collect()
}

struct Draft {
    algorithm: &'static str,
    modality: Modality,
    payload_kind: PayloadKind,
    tokens: Vec<TokenEntry>,
    statistics: BTreeMap<String, u64>,
    fingerprint: Option<String>,
    samples: Vec<Payload>,
}

/// Turn a draft into a sealed vocabulary with self-test vectors.
fn finish(draft: Draft, config: &TrainingConfig) -> Result<Table> {
    let mut tokens = draft.tokens;
    for name in &config.reserved_tokens {
        let id = tokens.len() as u32;
        let payload = match draft.payload_kind {
            PayloadKind::Bytes => TokenPayload::Bytes(name.as_bytes().to_vec()),
            PayloadKind::Symbols => TokenPayload::Symbols(vec![name.clone()]),
        };
        tokens.push(TokenEntry { id, kind: TokenKind::Special, payload, frequency: None });
    }
    let vocabulary = Vocabulary {
        manifest: Manifest {
            kvf_version: KVF_VERSION.to_string(),
            id: config.vocabulary_id.clone(),
            version: config.vocabulary_version.clone(),
            algorithm: draft.algorithm.to_string(),
            modality: draft.modality,
            payload_kind: draft.payload_kind,
            lossless: true,
            description: config.description.clone(),
            created: None,
            generator: format!("kute {KUTE_VERSION}"),
        },
        pretokenizer: None,
        adapter: None,
        tokens,
        merges: Vec::new(),
        statistics: draft.statistics,
        provenance: Provenance {
            corpus_fingerprints: draft.fingerprint.into_iter().collect(),
            license: config.license.clone(),
            training: Some(serde_json::to_value(config)?),
        },
        test_vectors: Vec::new(),
        representations: Vec::new(),
        integrity: None,
    };
    let mut table = Table::build(vocabulary, draft.algorithm)?;
    let mut vectors = Vec::new();
    for sample in draft.samples.into_iter().filter(|_| config.embed_test_vectors) {
        let input = CanonicalInput { modality: draft.modality, payload: sample };
        let ids = table.encode(&input)?;
        let input = match input.payload {
            Payload::Bytes(b) => TokenPayload::Bytes(b),
            Payload::Symbols(s) => TokenPayload::Symbols(s),
        };
        vectors.push(TestVector { input, ids });
    }
    table.vocabulary.test_vectors = vectors;
    table.vocabulary.seal()?;
    Ok(table)
}

fn prefix(bytes: &[u8], max: usize) -> Vec<u8> {
    let mut end = bytes.len().min(max);
    if std::str::from_utf8(bytes).is_ok() {
        while end > 0 && end < bytes.len() && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    bytes[..end].to_vec()
}

fn check_limit(config: &TrainingConfig, fingerprint: &CorpusFingerprint) -> Result<()> {
    match config.max_corpus_units {
        Some(limit) if fingerprint.units() > limit => {
            Err(KuteError::LimitExceeded(format!("corpus is larger than the max_corpus_units limit of {limit}")))
        }
        _ => Ok(()),
    }
}

macro_rules! table_tokenizer {
    ($name:ident, $algorithm:expr, $accepts:expr, $rejects:expr, $notes:expr) => {
        #[derive(Default)]
        pub struct $name {
            table: Option<Table>,
        }

        impl $name {
            fn table(&self) -> Result<&Table> {
                self.table.as_ref().ok_or_else(|| KuteError::NotTrained("train or load a vocabulary first".into()))
            }
        }

        impl Tokenizer for $name {
            fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary> {
                config.validate()?;
                let table = finish(Self::draft(corpus, config)?, config)?;
                let vocabulary = table.vocabulary.clone();
                self.table = Some(table);
                Ok(vocabulary)
            }

            fn load(&mut self, vocabulary: Vocabulary) -> Result<()> {
                self.table = Some(Table::build(vocabulary, $algorithm)?);
                Ok(())
            }

            fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence> {
                let table = self.table()?;
                sequence_for(&table.vocabulary, input.modality, table.encode(input)?)
            }

            fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
                self.table()?.decode(tokens)
            }

            fn vocabulary(&self) -> Option<&Vocabulary> {
                self.table.as_ref().map(|t| &t.vocabulary)
            }

            fn capabilities(&self) -> TokenizerCapabilities {
                TokenizerCapabilities {
                    algorithm: $algorithm.to_string(),
                    lossless: true,
                    trainable: true,
                    deterministic: true,
                    accepts: $accepts,
                    rejects_unknown: $rejects,
                    notes: $notes.to_string(),
                }
            }
        }
    };
}

table_tokenizer!(
    ByteTokenizer,
    BYTES,
    vec![PayloadKind::Bytes],
    false,
    "One token per byte. Needs no training data; the lossless baseline."
);
table_tokenizer!(
    CharTokenizer,
    CHAR,
    vec![PayloadKind::Bytes],
    false,
    "One token per Unicode character seen in training, with byte tokens as the fallback for everything else."
);
table_tokenizer!(
    DictionaryTokenizer,
    DICTIONARY,
    vec![PayloadKind::Bytes, PayloadKind::Symbols],
    false,
    "Fixed entries. Over bytes: longest match with byte fallback, always lossless. Over symbols: lookup that refuses unknown symbols."
);

impl ByteTokenizer {
    /// A ready byte tokenizer for `config`; no corpus needed.
    pub fn with_config(config: &TrainingConfig) -> Result<Self> {
        let mut t = Self::default();
        t.train(&mut kute_core::VecCorpus::new(Vec::new()), config)?;
        Ok(t)
    }

    fn draft(corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Draft> {
        let mut fingerprint = CorpusFingerprint::new();
        while let Some(record) = corpus.next_record()? {
            if record.payload.kind() != PayloadKind::Bytes {
                return Err(KuteError::InvalidInput("the bytes algorithm takes byte payloads".into()));
            }
            fingerprint.update(&record);
            check_limit(config, &fingerprint)?;
        }
        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        statistics.insert("corpus_bytes".to_string(), fingerprint.units());
        let seen = fingerprint.records() > 0;
        Ok(Draft {
            algorithm: BYTES,
            modality: Modality::Binary,
            payload_kind: PayloadKind::Bytes,
            tokens: byte_tokens(),
            statistics,
            fingerprint: seen.then(|| fingerprint.finish()),
            samples: vec![Payload::Bytes(vec![0x00, 0x7f, 0x80, 0xff, b'K'])],
        })
    }
}

impl CharTokenizer {
    fn draft(corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Draft> {
        let mut fingerprint = CorpusFingerprint::new();
        let mut counts: BTreeMap<char, u64> = BTreeMap::new();
        let mut samples = Vec::new();
        while let Some(record) = corpus.next_record()? {
            let Payload::Bytes(bytes) = &record.payload else {
                return Err(KuteError::InvalidInput("the char algorithm takes byte payloads".into()));
            };
            fingerprint.update(&record);
            check_limit(config, &fingerprint)?;
            if samples.len() < 4 && !bytes.is_empty() {
                samples.push(Payload::Bytes(prefix(bytes, 96)));
            }
            for chunk in bytes.utf8_chunks() {
                for c in chunk.valid().chars() {
                    if c.len_utf8() > 1 {
                        *counts.entry(c).or_insert(0) += 1;
                    }
                }
            }
        }
        // Most frequent first; ties by code point. Truncate to the budget.
        let mut chars: Vec<(char, u64)> = counts.into_iter().filter(|(_, n)| *n >= config.min_frequency).collect();
        chars.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let reserved = config.reserved_tokens.len();
        let budget = (config.vocab_size as usize).saturating_sub(256 + reserved);
        if (config.vocab_size as usize) < 256 + reserved {
            return Err(KuteError::InvalidConfig("the char algorithm needs vocab_size of at least 256 plus reserved tokens".into()));
        }
        let dropped = chars.len().saturating_sub(budget) as u64;
        chars.truncate(budget);
        let mut tokens = byte_tokens();
        for (c, n) in &chars {
            let id = tokens.len() as u32;
            tokens.push(TokenEntry { id, kind: TokenKind::Char, payload: TokenPayload::Bytes(c.to_string().into_bytes()), frequency: Some(*n) });
        }
        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        statistics.insert("corpus_bytes".to_string(), fingerprint.units());
        statistics.insert("characters_kept".to_string(), chars.len() as u64);
        statistics.insert("characters_left_to_byte_fallback".to_string(), dropped);
        Ok(Draft {
            algorithm: CHAR,
            modality: Modality::Text,
            payload_kind: PayloadKind::Bytes,
            tokens,
            statistics,
            fingerprint: Some(fingerprint.finish()),
            samples,
        })
    }
}

impl DictionaryTokenizer {
    /// With `config.dictionary` set, the entries are exactly that list. With it
    /// empty and a symbol corpus, the dictionary is the corpus alphabet.
    fn draft(corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Draft> {
        let mut fingerprint = CorpusFingerprint::new();
        let mut kind: Option<PayloadKind> = None;
        let mut alphabet: BTreeMap<String, u64> = BTreeMap::new();
        let mut samples = Vec::new();
        while let Some(record) = corpus.next_record()? {
            fingerprint.update(&record);
            check_limit(config, &fingerprint)?;
            let this = record.payload.kind();
            if *kind.get_or_insert(this) != this {
                return Err(KuteError::InvalidInput("a dictionary corpus must be all bytes or all symbols".into()));
            }
            match &record.payload {
                Payload::Symbols(s) => {
                    for sym in s {
                        *alphabet.entry(sym.clone()).or_insert(0) += 1;
                    }
                    if samples.len() < 4 && !s.is_empty() {
                        samples.push(Payload::Symbols(s.iter().take(32).cloned().collect()));
                    }
                }
                Payload::Bytes(b) => {
                    if samples.len() < 4 && !b.is_empty() {
                        samples.push(Payload::Bytes(prefix(b, 96)));
                    }
                }
            }
        }
        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        let seen = fingerprint.records() > 0;
        let units = fingerprint.units();
        let fingerprint = seen.then(|| fingerprint.finish());
        let reserved = config.reserved_tokens.len();

        if kind == Some(PayloadKind::Symbols) {
            let entries: Vec<(String, Option<u64>)> = if config.dictionary.is_empty() {
                alphabet.iter().filter(|(_, n)| **n >= config.min_frequency).map(|(s, n)| (s.clone(), Some(*n))).collect()
            } else {
                unique(&config.dictionary)?.into_iter().map(|s| { let n = alphabet.get(&s).copied(); (s, n) }).collect()
            };
            if entries.is_empty() {
                return Err(KuteError::InvalidInput("no dictionary entries: the corpus is empty and no dictionary was given".into()));
            }
            if entries.len() + reserved > config.vocab_size as usize {
                return Err(KuteError::InvalidConfig(format!("{} entries do not fit in vocab_size {}", entries.len(), config.vocab_size)));
            }
            statistics.insert("corpus_symbols".to_string(), units);
            statistics.insert("entries".to_string(), entries.len() as u64);
            let tokens = entries
                .into_iter()
                .enumerate()
                .map(|(i, (s, n))| TokenEntry { id: i as u32, kind: TokenKind::Symbol, payload: TokenPayload::Symbols(vec![s]), frequency: n })
                .collect();
            return Ok(Draft { algorithm: DICTIONARY, modality: Modality::Events, payload_kind: PayloadKind::Symbols, tokens, statistics, fingerprint, samples });
        }

        if config.dictionary.is_empty() {
            return Err(KuteError::InvalidConfig("a byte dictionary needs explicit entries (TrainingConfig.dictionary)".into()));
        }
        let entries = unique(&config.dictionary)?;
        let mut tokens = byte_tokens();
        for e in &entries {
            if e.len() < 2 {
                continue; // single bytes are already tokens
            }
            let id = tokens.len() as u32;
            tokens.push(TokenEntry { id, kind: TokenKind::Entry, payload: TokenPayload::Bytes(e.as_bytes().to_vec()), frequency: None });
        }
        if tokens.len() + reserved > config.vocab_size as usize {
            return Err(KuteError::InvalidConfig(format!("{} tokens do not fit in vocab_size {}", tokens.len() + reserved, config.vocab_size)));
        }
        statistics.insert("corpus_bytes".to_string(), units);
        statistics.insert("entries".to_string(), (tokens.len() - 256) as u64);
        Ok(Draft { algorithm: DICTIONARY, modality: Modality::Text, payload_kind: PayloadKind::Bytes, tokens, statistics, fingerprint, samples })
    }
}

fn unique(entries: &[String]) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    for e in entries {
        if e.is_empty() {
            return Err(KuteError::InvalidConfig("dictionary entries must not be empty".into()));
        }
        if !seen.insert(e.as_str()) {
            return Err(KuteError::InvalidConfig(format!("dictionary entry {e:?} is listed twice")));
        }
    }
    Ok(entries.to_vec())
}
