//! Native byte-level BPE. Ids 0..=255 are the byte values, learned merges
//! follow, reserved tokens come last. Lossless for arbitrary bytes.

use crate::merge::{apply_merges, resumable_merges, train_merges, LearnedMerge, Pair};
use fancy_regex::Regex;
use rayon::prelude::*;
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
    regex: Option<Splitter>,
}

/// How a pre-tokenizer pattern is executed.
pub(crate) enum Splitter {
    /// Any pattern, on the backtracking engine.
    Fancy(Regex),
    /// The GPT-4 preset on the automaton engine. The preset's possessive
    /// quantifiers never change what it matches (each guards a character class
    /// disjoint from what follows), so they are dropped; its one lookahead,
    /// `\s+(?!\S)`, is applied by hand in `for_each_chunk`. The differential
    /// test in this file holds the two engines to identical chunks.
    Gpt4(regex::Regex),
}

const GPT4_FAST: &str = r"'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]|\s+";

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

pub(crate) fn compile(pattern: Option<&str>) -> Result<Option<Splitter>> {
    match pattern {
        None => Ok(None),
        Some(p) if p == kute_types::config::GPT4_PATTERN => regex::Regex::new(GPT4_FAST)
            .map(|re| Some(Splitter::Gpt4(re)))
            .map_err(|e| KuteError::InvalidConfig(format!("pretokenizer pattern does not compile: {e}"))),
        Some(p) => compile_fancy(p).map(|re| Some(Splitter::Fancy(re))),
    }
}

fn compile_fancy(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|e| KuteError::InvalidConfig(format!("pretokenizer pattern does not compile: {e}")))
}

/// Split `bytes` into chunks and hand each to `f`. Every byte is delivered
/// exactly once and in order, whatever the pattern matches, so chunking can
/// never lose information: text the pattern skips becomes its own chunk, and
/// input that is not valid UTF-8 (or that the regex engine gives up on) is
/// delivered whole.
pub(crate) fn for_each_chunk<'a>(splitter: Option<&Splitter>, bytes: &'a [u8], mut f: impl FnMut(&'a [u8])) {
    if bytes.is_empty() {
        return;
    }
    let (Some(splitter), Ok(text)) = (splitter, std::str::from_utf8(bytes)) else {
        f(bytes);
        return;
    };
    let re = match splitter {
        Splitter::Fancy(re) => re,
        Splitter::Gpt4(re) => {
            let mut at = 0;
            while at < text.len() {
                let Some(m) = re.find_at(text, at) else { break };
                let (start, mut end) = (m.start(), m.end());
                if end <= start {
                    break;
                }
                // `\s+(?!\S)`: a whitespace run that stops before a non-space
                // gives its last character back. Only the `\s+` branch can
                // end in whitespace other than a line break.
                if end < text.len() {
                    if let Some(c) = text[start..end].chars().next_back() {
                        if c.is_whitespace() && c != '\n' && c != '\r' && end - start > c.len_utf8() {
                            end -= c.len_utf8();
                        }
                    }
                }
                if start > at {
                    f(&bytes[at..start]);
                }
                f(&bytes[start..end]);
                at = end;
            }
            if at < bytes.len() {
                f(&bytes[at..]);
            }
            return;
        }
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

/// Inputs at least this long are encoded in pieces on several threads.
const PARALLEL_MIN_BYTES: usize = 1 << 20;
const PARALLEL_PIECE_BYTES: usize = 1 << 18;

/// Cut points for encoding a long text in parallel under the GPT-4 preset.
///
/// A cut is placed only between a line feed and an ASCII letter. No branch of
/// the preset can match across that position (line breaks only ever end a
/// match, and the one branch that starts with an optional non-letter excludes
/// line breaks), and the preset has no look-behind, so the pieces tokenize
/// exactly as the whole does.
fn parallel_cuts(bytes: &[u8]) -> Vec<usize> {
    let mut cuts = vec![0];
    let mut target = PARALLEL_PIECE_BYTES;
    while target < bytes.len() {
        match (target..bytes.len()).find(|&i| bytes[i - 1] == b'\n' && bytes[i].is_ascii_alphabetic()) {
            Some(i) => {
                cuts.push(i);
                target = i + PARALLEL_PIECE_BYTES;
            }
            None => break,
        }
    }
    cuts.push(bytes.len());
    cuts
}

fn encode_bytes(loaded: &Loaded, bytes: &[u8]) -> Vec<u32> {
    if bytes.len() >= PARALLEL_MIN_BYTES && matches!(loaded.regex, Some(Splitter::Gpt4(_))) && std::str::from_utf8(bytes).is_ok() {
        let cuts = parallel_cuts(bytes);
        if cuts.len() > 2 {
            let pieces: Vec<Vec<u32>> = cuts.par_windows(2).map(|w| encode_piece(loaded, &bytes[w[0]..w[1]])).collect();
            let mut out = Vec::with_capacity(pieces.iter().map(Vec::len).sum());
            for p in &pieces {
                out.extend_from_slice(p);
            }
            return out;
        }
    }
    encode_piece(loaded, bytes)
}

fn encode_piece(loaded: &Loaded, bytes: &[u8]) -> Vec<u32> {
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
#[doc(hidden)]
pub fn build_vocabulary(
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
        let mut words: Vec<Vec<u32>> = chunks.into_iter().map(|(c, _)| c.into_iter().map(u32::from).collect()).collect();

        let max_merges = config.vocab_size - 256 - reserved;
        let mut lengths: Vec<usize> = vec![1; 256];
        let limit = config.max_token_length;
        let corpus_records = fingerprint.records();
        let corpus_units = fingerprint.units();
        let corpus_fingerprint = fingerprint.finish();

        // Resuming: bring the words to the state the earlier run left them
        // in. Which pair is merged next depends only on that state, so
        // learning continues exactly as it would have.
        let mut kept: Vec<LearnedMerge> = Vec::new();
        if let Some(prior) = &config.resume_from {
            kept = resumable_merges(prior, ALGORITHM, config, &corpus_fingerprint, 256, max_merges)?;
            let mut ranks: FxHashMap<Pair, u32> = FxHashMap::default();
            for m in &kept {
                ranks.insert((m.left, m.right), m.id);
                lengths.push(lengths[m.left as usize] + lengths[m.right as usize]);
            }
            words.par_iter_mut().for_each(|w| apply_merges(w, &ranks));
        }
        let resumed = kept.len() as u32;
        let more = train_merges(words, &counts, 256 + resumed, max_merges - resumed, config.min_frequency, |l, r| {
            let len = lengths[l as usize] + lengths[r as usize];
            if limit.is_none_or(|m| len <= m) && len <= kute_types::vocab::MAX_TOKEN_BYTES {
                lengths.push(len);
                true
            } else {
                false
            }
        });

        let mut learned = kept;
        learned.extend(more);

        let mut statistics = BTreeMap::new();
        statistics.insert("corpus_records".to_string(), corpus_records);
        statistics.insert("corpus_bytes".to_string(), corpus_units);
        statistics.insert("unique_chunks".to_string(), unique_chunks);
        statistics.insert("merges_learned".to_string(), learned.len() as u64);
        statistics.insert("requested_vocab_size".to_string(), config.vocab_size as u64);
        let merges: Vec<(u32, u32, u32, Option<u64>)> = learned.iter().map(|m| (m.left, m.right, m.id, Some(m.count))).collect();
        let vocabulary = build_vocabulary(ALGORITHM, config, &merges, statistics, corpus_fingerprint)?;
        self.finish_training(vocabulary, samples, config.embed_test_vectors)
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
    fn finish_training(&mut self, vocabulary: Vocabulary, samples: Vec<Vec<u8>>, embed: bool) -> Result<Vocabulary> {
        self.load(vocabulary)?;
        let loaded = self.loaded.as_mut().ok_or_else(|| KuteError::NotTrained("load failed".into()))?;
        let vectors: Vec<TestVector> = samples
            .into_iter()
            .filter(|_| embed)
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
#[doc(hidden)]
pub fn sample_prefix(bytes: &[u8], max: usize) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use kute_types::config::GPT4_PATTERN;
    use kute_types::VocabularyId;

    fn chunks(splitter: &Splitter, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for_each_chunk(Some(splitter), text.as_bytes(), |c| out.push(String::from_utf8(c.to_vec()).unwrap()));
        out
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// The automaton path for the GPT-4 preset must split exactly as the
    /// backtracking engine does on the preset as written.
    #[test]
    fn fast_gpt4_splitter_matches_the_pattern() {
        let Some(fast @ Splitter::Gpt4(_)) = compile(Some(GPT4_PATTERN)).unwrap() else { panic!("preset should take the fast path") };
        let reference = Splitter::Fancy(compile_fancy(GPT4_PATTERN).unwrap());
        let mut cases: Vec<String> = [
            "", " ", "  ", "   a", "a   ", "a \n b", "a  \n\n  b", "\n\n\n", " \n", "\r\n\r\n x", "don't we'll I'VE it'S 'tis ''", "x = 12345.678e-9;",
            "tabs\t\tand\t spaces  \t", "end with spaces   ", "naïve café 日本語 テスト", "emoji 🙂🙂 ok", "nbsp\u{a0}\u{a0}x \u{2028} y\u{3000}\u{3000}z",
            "e\u{301}tude \u{301}\u{301}", "٣٤٥ ١٢٣٤ ⅷ", "fn main() {\n    println!(\"{}\", 1);\n}\n", "a\n\nb\n \nc \n", "!!!\n\n\nabc", " !a", "  !", "\t\n\t\nx",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let alphabet: Vec<char> = "abcXYZ 012  \t\n\r'.,!?-_(){}\u{a0}\u{2028}\u{3000}é日ß\u{301}٣🙂".chars().collect();
        let mut state = 0x2545f4914f6cdd1du64;
        for _ in 0..4000 {
            let len = xorshift(&mut state) % 40;
            cases.push((0..len).map(|_| alphabet[(xorshift(&mut state) % alphabet.len() as u64) as usize]).collect());
        }
        for text in &cases {
            let (a, b) = (chunks(&fast, text), chunks(&reference, text));
            assert_eq!(a, b, "input {text:?}");
            assert_eq!(a.concat(), *text);
        }
    }

    /// Encoding a long text in parallel pieces gives the ids of one serial pass.
    #[test]
    fn parallel_encode_matches_serial() {
        let words = ["alpha", "beta", "gamma", "delta", "x1", "42", "naïve", "日本", "don't", "--", "\t"];
        let mut state = 0x9e3779b97f4a7c15u64;
        let mut text = String::new();
        while text.len() < (PARALLEL_MIN_BYTES * 5) / 2 {
            for _ in 0..(1 + xorshift(&mut state) % 14) {
                text.push_str(words[(xorshift(&mut state) % words.len() as u64) as usize]);
                text.push_str(if xorshift(&mut state) % 9 == 0 { "  " } else { " " });
            }
            text.push_str(match xorshift(&mut state) % 5 { 0 => "\n\n", 1 => " \n", 2 => "\r\n", _ => "\n" });
        }
        let mut config = TrainingConfig::new(VocabularyId::parse("test:parallel").unwrap(), 400);
        config.pretokenizer = kute_types::Pretokenizer::Gpt4;
        let mut tokenizer = BpeTokenizer::new();
        let sample = CanonicalInput::bytes(Modality::Text, text.as_bytes()[..60_000].to_vec());
        tokenizer.train(&mut kute_core::VecCorpus::new(vec![sample]), &config).unwrap();
        let loaded = tokenizer.loaded().unwrap();
        assert!(parallel_cuts(text.as_bytes()).len() > 3, "the text should be cut into several pieces");
        assert_eq!(encode_bytes(loaded, text.as_bytes()), encode_piece(loaded, text.as_bytes()));
    }
}
