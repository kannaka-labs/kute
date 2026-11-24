//! Native byte-level BPE. Ids 0..=255 are the byte values, learned merges
//! follow, reserved tokens come last. Lossless for arbitrary bytes.

use crate::merge::{apply_merges, train_merges, Pair};
use fancy_regex::Regex;
use kute_core::{check_sequence, sequence_for, CorpusFingerprint, CorpusSource, Tokenizer};
use kute_types::{
    CanonicalInput, KuteError, Manifest, MergeRule, Modality, Payload, PayloadKind, Provenance, Result,
    TestVector, TokenEntry, TokenKind, TokenPayload, TokenSequence, TokenizerCapabilities, TrainingConfig,
    Vocabulary, KUTE_VERSION, KVF_VERSION,
};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;

pub const ALGORITHM: &str = "bpe";
/// Vocabularies written by the rustbpe backend use the same table layout and
/// are read by this tokenizer too.
pub const COMPATIBLE_ALGORITHMS: &[&str] = &["bpe", "rustbpe"];

struct Loaded {
    vocabulary: Vocabulary,
    ranks: FxHashMap<Pair, u32>,
    regex: Option<Regex>,
}

#[derive(Default)]
pub struct BpeTokenizer {
    loaded: Option<Loaded>,
}

impl BpeTokenizer {
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

pub(crate) fn compile(pattern: Option<&str>) -> Result<Option<Regex>> {
    match pattern {
        None => Ok(None),
        Some(p) => Regex::new(p)
            .map(Some)
            .map_err(|e| KuteError::InvalidConfig(format!("pretokenizer pattern does not compile: {e}"))),
    }
}

/// Split `bytes` into chunks and hand each to `f`. Every byte is delivered
/// exactly once and in order, whatever the pattern matches, so chunking can
/// never lose information: text the pattern skips becomes its own chunk, and
/// input that is not valid UTF-8 (or that the regex engine gives up on) is
/// delivered whole.
pub(crate) fn for_each_chunk<'a>(regex: Option<&Regex>, bytes: &'a [u8], mut f: impl FnMut(&'a [u8])) {
    if bytes.is_empty() {
        return;
    }
    let (Some(re), Ok(text)) = (regex, std::str::from_utf8(bytes)) else {
        f(bytes);
        return;
    };
    let mut last = 0;
    for m in re.find_iter(text) {
        let Ok(m) = m else { break };
        if m.end() <= m.start() || m.start() < last {
            continue;
        }
        if m.start() > last {
            f(&bytes[last..m.start()]);
        }
        f(&bytes[m.start()..m.end()]);
        last = m.end();
    }
    if last < bytes.len() {
        f(&bytes[last..]);
    }
}

fn encode_bytes(loaded: &Loaded, bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut scratch: Vec<u32> = Vec::new();
    // Words repeat, so remember where each short chunk's ids already sit in
    // `out`. The cache lives for one call and never changes the result.
    const CACHE_CHUNK_MAX: usize = 48;
    const CACHE_ENTRIES_MAX: usize = 1 << 16;
    let cacheable = loaded.regex.is_some();
    let mut cache: FxHashMap<&[u8], (usize, usize)> = FxHashMap::default();
    for_each_chunk(loaded.regex.as_ref(), bytes, |chunk| {
        let cache_this = cacheable && chunk.len() <= CACHE_CHUNK_MAX;
        if cache_this {
            if let Some(&(start, len)) = cache.get(chunk) {
                out.extend_from_within(start..start + len);
                return;
            }
        }
        scratch.clear();
        scratch.extend(chunk.iter().map(|&b| b as u32));
        apply_merges(&mut scratch, &loaded.ranks);
        if cache_this && cache.len() < CACHE_ENTRIES_MAX {
            cache.insert(chunk, (out.len(), scratch.len()));
        }
        out.extend_from_slice(&scratch);
    });
    out
}

/// Assemble a byte-BPE vocabulary from learned merges. Shared with the
/// rustbpe backend so both write identical tables.
pub(crate) fn build_vocabulary(
    algorithm: &str,
    config: &TrainingConfig,
    merges: &[(u32, u32, u32, Option<u64>)],
    statistics: BTreeMap<String, u64>,
    fingerprint: String,
) -> Result<Vocabulary> {
    let mut tokens: Vec<TokenEntry> = (0u32..256)
        .map(|b| TokenEntry { id: b, kind: TokenKind::Byte, payload: TokenPayload::Bytes(vec![b as u8]), frequency: None })
        .collect();
    let mut rules = Vec::with_capacity(merges.len());
    for &(left, right, id, count) in merges {
        if id as usize != tokens.len() || left >= id || right >= id {
            return Err(KuteError::Backend(format!("merge ({left}, {right}) -> {id} is out of order")));
        }
        let (TokenPayload::Bytes(a), TokenPayload::Bytes(b)) = (&tokens[left as usize].payload, &tokens[right as usize].payload) else {
            return Err(KuteError::Backend("byte BPE produced a non-byte token".into()));
        };
        let mut bytes = a.clone();
        bytes.extend_from_slice(b);
        tokens.push(TokenEntry { id, kind: TokenKind::Merge, payload: TokenPayload::Bytes(bytes), frequency: count });
        rules.push(MergeRule { left, right, id });
    }
    for name in &config.reserved_tokens {
        let id = tokens.len() as u32;
        tokens.push(TokenEntry { id, kind: TokenKind::Special, payload: TokenPayload::Bytes(name.as_bytes().to_vec()), frequency: None });
    }
    let modality = if config.pretokenizer.pattern().is_some() { Modality::Text } else { Modality::Binary };
    Ok(Vocabulary {
        manifest: Manifest {
            kvf_version: KVF_VERSION.to_string(),
            id: config.vocabulary_id.clone(),
            version: config.vocabulary_version.clone(),
            algorithm: algorithm.to_string(),
            modality,
            payload_kind: PayloadKind::Bytes,
            lossless: true,
            description: config.description.clone(),
            created: None,
            generator: format!("kute {KUTE_VERSION}"),
        },
        pretokenizer: config.pretokenizer.pattern().map(str::to_string),
        adapter: None,
        tokens,
        merges: rules,
        statistics,
        provenance: Provenance {
            corpus_fingerprints: vec![fingerprint],
            license: config.license.clone(),
            training: Some(serde_json::to_value(config)?),
        },
        test_vectors: Vec::new(),
        representations: Vec::new(),
        integrity: None,
    })
}

impl Tokenizer for BpeTokenizer {
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary> {
        config.validate()?;
        let reserved = config.reserved_tokens.len() as u32;
        if config.vocab_size < 256 + reserved {
            return Err(KuteError::InvalidConfig(format!(
                "byte BPE needs vocab_size of at least {} (256 bytes plus {reserved} reserved tokens)",
                256 + reserved
            )));
        }
        let regex = compile(config.pretokenizer.pattern())?;
        let mut fingerprint = CorpusFingerprint::new();
        let mut chunk_counts: FxHashMap<Vec<u8>, u64> = FxHashMap::default();
        let mut samples: Vec<Vec<u8>> = Vec::new();
        while let Some(record) = corpus.next_record()? {
            let Payload::Bytes(bytes) = &record.payload else {
                return Err(KuteError::InvalidInput("the bpe algorithm takes byte payloads; use sequence-bpe for symbols".into()));
            };
            fingerprint.update(&record);
            if let Some(limit) = config.max_corpus_units {
                if fingerprint.units() > limit {
                    return Err(KuteError::LimitExceeded(format!("corpus is larger than the max_corpus_units limit of {limit}")));
                }
            }
            if samples.len() < 4 && !bytes.is_empty() {
                samples.push(sample_prefix(bytes, 96));
            }
            let windowed = regex.is_none() || std::str::from_utf8(bytes).is_err();
            if windowed {
                for window in bytes.chunks(config.train_window) {
                    *chunk_counts.entry(window.to_vec()).or_insert(0) += 1;
                }
            } else {
                for_each_chunk(regex.as_ref(), bytes, |chunk| {
                    // A pattern that matches nothing would hand back the whole
                    // record; keep counting bounded either way.
                    for window in chunk.chunks(config.train_window.max(4096)) {
                        *chunk_counts.entry(window.to_vec()).or_insert(0) += 1;
                    }
                });
            }
        }

        let mut chunks: Vec<(Vec<u8>, u64)> = chunk_counts.into_iter().collect();
        chunks.sort_unstable();
        let unique_chunks = chunks.len() as u64;
        let counts: Vec<u64> = chunks.iter().map(|(_, c)| *c).collect();
        let words: Vec<Vec<u32>> = chunks.into_iter().map(|(c, _)| c.into_iter().map(u32::from).collect()).collect();

        let max_merges = config.vocab_size - 256 - reserved;
        let mut lengths: Vec<usize> = vec![1; 256];
        let limit = config.max_token_length;
        let learned = train_merges(words, &counts, 256, max_merges, config.min_frequency, |l, r| {
            let len = lengths[l as usize] + lengths[r as usize];
            if limit.map_or(true, |m| len <= m) && len <= kute_types::vocab::MAX_TOKEN_BYTES {
                lengths.push(len);
                true
            } else {
                false
            }
        });

        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), fingerprint.records());
        statistics.insert("corpus_bytes".to_string(), fingerprint.units());
        statistics.insert("unique_chunks".to_string(), unique_chunks);
        statistics.insert("merges_learned".to_string(), learned.len() as u64);
        statistics.insert("requested_vocab_size".to_string(), config.vocab_size as u64);
        let merges: Vec<(u32, u32, u32, Option<u64>)> = learned.iter().map(|m| (m.left, m.right, m.id, Some(m.count))).collect();
        let vocabulary = build_vocabulary(ALGORITHM, config, &merges, statistics, fingerprint.finish())?;
        self.finish_training(vocabulary, samples)
    }

    fn load(&mut self, mut vocabulary: Vocabulary) -> Result<()> {
        vocabulary.validate()?;
        if !COMPATIBLE_ALGORITHMS.contains(&vocabulary.manifest.algorithm.as_str()) {
            return Err(KuteError::VocabularyMismatch(format!(
                "the bpe tokenizer cannot interpret a {:?} vocabulary",
                vocabulary.manifest.algorithm
            )));
        }
        if vocabulary.manifest.payload_kind != PayloadKind::Bytes || vocabulary.tokens.len() < 256 {
            return Err(KuteError::InvalidVocabulary("byte BPE needs the 256 byte tokens first".into()));
        }
        for (i, t) in vocabulary.tokens.iter().take(256).enumerate() {
            if t.kind != TokenKind::Byte || t.payload != TokenPayload::Bytes(vec![i as u8]) {
                return Err(KuteError::InvalidVocabulary(format!("token {i} must be the byte {i}")));
            }
        }
        let mut ranks: FxHashMap<Pair, u32> = FxHashMap::default();
        for m in &vocabulary.merges {
            if ranks.insert((m.left, m.right), m.id).is_some() {
                return Err(KuteError::InvalidVocabulary(format!("pair ({}, {}) has two merge rules", m.left, m.right)));
            }
        }
        let regex = compile(vocabulary.pretokenizer.as_deref()).map_err(|e| KuteError::InvalidVocabulary(e.to_string()))?;
        if vocabulary.integrity.is_none() {
            vocabulary.seal()?;
        }
        self.loaded = Some(Loaded { vocabulary, ranks, regex });
        Ok(())
    }

    fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence> {
        let loaded = self.loaded()?;
        let Payload::Bytes(bytes) = &input.payload else {
            return Err(KuteError::InvalidInput("the bpe algorithm takes byte payloads".into()));
        };
        sequence_for(&loaded.vocabulary, input.modality, encode_bytes(loaded, bytes))
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput> {
        let loaded = self.loaded()?;
        check_sequence(&loaded.vocabulary, tokens)?;
        let mut out = Vec::with_capacity(tokens.ids.len() * 3);
        for &id in &tokens.ids {
            match &loaded.vocabulary.token(id)?.payload {
                TokenPayload::Bytes(b) => out.extend_from_slice(b),
                TokenPayload::Symbols(_) => return Err(KuteError::InvalidVocabulary("symbol token in a byte vocabulary".into())),
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
            rejects_unknown: false,
            notes: "Native byte-level BPE. Every byte value is a token, so any input encodes and decodes exactly.".to_string(),
        }
    }
}

impl BpeTokenizer {
    /// Install the trained vocabulary, record self-test vectors, and seal it.
    fn finish_training(&mut self, vocabulary: Vocabulary, samples: Vec<Vec<u8>>) -> Result<Vocabulary> {
        self.load(vocabulary)?;
        let loaded = self.loaded.as_mut().ok_or_else(|| KuteError::NotTrained("load failed".into()))?;
        let vectors: Vec<TestVector> = samples
            .into_iter()
            .map(|s| {
                let ids = encode_bytes(loaded, &s);
                TestVector { input: TokenPayload::Bytes(s), ids }
            })
            .collect();
        loaded.vocabulary.test_vectors = vectors;
        loaded.vocabulary.seal()?;
        Ok(loaded.vocabulary.clone())
    }
}

/// Up to `max` leading bytes, cut on a UTF-8 boundary when the data is text.
pub(crate) fn sample_prefix(bytes: &[u8], max: usize) -> Vec<u8> {
    if bytes.len() <= max {
        return bytes.to_vec();
    }
    let mut end = max;
    if std::str::from_utf8(bytes).is_ok() {
        while end > 0 && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    bytes[..end.max(1)].to_vec()
}
