//! Kannaka Universal Tokenization Engine.
//!
//! [`Engine`] wires the built-in algorithms and adapters into one registry and
//! offers the operations the CLI, the Python bindings and the benchmarks share:
//! train through an adapter, encode and decode raw bytes, verify a vocabulary
//! against its own test vectors.
//!
//! ```
//! use kute::{Engine, TrainingConfig, VocabularyId, Pretokenizer, AdapterConfig};
//!
//! let engine = Engine::new();
//! let mut config = TrainingConfig::new(VocabularyId::parse("example:greetings").unwrap(), 300);
//! config.pretokenizer = Pretokenizer::Gpt4;
//! let corpus = vec![b"hello hello hello world".to_vec()];
//! let vocab = engine.train("bpe", "text", &AdapterConfig::default(), &corpus, &config).unwrap();
//! let tokenizer = engine.tokenizer(vocab).unwrap();
//! let tokens = engine.encode_raw(tokenizer.as_ref(), b"hello world").unwrap();
//! assert_eq!(engine.decode_raw(tokenizer.as_ref(), &tokens).unwrap(), b"hello world");
//! ```

use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub use kute_adapters as adapters;
pub use kute_algorithms as algorithms;
pub use kute_bpe as bpe;
pub use kute_core::{
    check_sequence, decoded_units, AdapterCapabilities, CorpusSource, FnCorpus, ModalityAdapter, Registry, Representation,
    RepresentationBackend, Tokenizer, VecCorpus,
};
pub use kute_types::*;
pub use kute_vocab as vocab;

/// The engine: a registry plus the common workflows.
#[derive(Clone)]
pub struct Engine {
    registry: Registry,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of running a vocabulary's embedded test vectors.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyReport {
    pub vocabulary: String,
    pub digest: String,
    pub vectors: usize,
    pub passed: usize,
    pub failures: Vec<String>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

impl Engine {
    /// An engine with every built-in algorithm and adapter registered.
    pub fn new() -> Self {
        let mut registry = Registry::new();
        // The built-in names are distinct, so these cannot collide.
        kute_bpe::register(&mut registry).expect("built-in algorithm names are unique");
        kute_algorithms::register(&mut registry).expect("built-in algorithm names are unique");
        kute_adapters::register(&mut registry).expect("built-in adapter names are unique");
        Self { registry }
    }

    /// An engine around a caller-built registry.
    pub fn with_registry(registry: Registry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Add algorithms or adapters without touching KUTE's own crates.
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// Train `algorithm` on raw documents read through `adapter`.
    ///
    /// The adapter and its options are recorded in the vocabulary, so encoding
    /// and decoding later go through the same adapter without being told.
    pub fn train(
        &self,
        algorithm: &str,
        adapter: &str,
        adapter_config: &AdapterConfig,
        documents: &[Vec<u8>],
        config: &TrainingConfig,
    ) -> Result<Vocabulary> {
        let mut next = documents.iter();
        self.train_stream(algorithm, adapter, adapter_config, config, &mut || Ok(next.next().map(std::borrow::Cow::Borrowed)))
    }

    /// Train on documents that are produced one at a time. Only the
    /// document being read is held, so a corpus larger than memory can be
    /// trained on as long as each document fits.
    pub fn train_stream<'a>(
        &self,
        algorithm: &str,
        adapter: &str,
        adapter_config: &AdapterConfig,
        config: &TrainingConfig,
        documents: &mut dyn FnMut() -> Result<Option<std::borrow::Cow<'a, Vec<u8>>>>,
    ) -> Result<Vocabulary> {
        let adapter_impl = self.registry.adapter(adapter)?;
        let mut tokenizer = self.registry.tokenizer(algorithm)?;
        let mut corpus = FnCorpus(|| match documents()? {
            Some(doc) => adapter_impl.ingest(&doc, adapter_config).map(Some),
            None => Ok(None),
        });
        let mut vocabulary = tokenizer.train(&mut corpus, config)?;
        vocabulary.adapter = Some(adapter_impl.spec(adapter_config));
        vocabulary.manifest.modality = adapter_impl.modality();
        vocabulary.seal()?;
        // Fail here, not at first use, if the result is not self-consistent.
        let report = self.verify(&vocabulary)?;
        if !report.ok() {
            return Err(KuteError::Backend(format!("trained vocabulary fails its own test vectors: {}", report.failures.join("; "))));
        }
        Ok(vocabulary)
    }

    /// Train on files. A directory is read recursively, in sorted path order.
    pub fn train_paths(
        &self,
        algorithm: &str,
        adapter: &str,
        adapter_config: &AdapterConfig,
        paths: &[PathBuf],
        config: &TrainingConfig,
    ) -> Result<Vocabulary> {
        let mut files = Vec::new();
        for path in paths {
            files.extend(collect_files(path)?);
        }
        if files.is_empty() {
            return Err(KuteError::InvalidInput("no input files found".into()));
        }
        if config.stream_bytes.is_some() && adapter == kute_adapters::JSON {
            return Err(KuteError::InvalidConfig("the json adapter reads whole documents and cannot be streamed in pieces".into()));
        }
        // Files are read one at a time, or one piece at a time, so memory
        // does not grow with the size of the corpus.
        let line_cut = adapter != kute_adapters::BINARY;
        let mut files = files.into_iter();
        let mut open: Option<Pieces> = None;
        self.train_stream(algorithm, adapter, adapter_config, config, &mut || loop {
            if let Some(pieces) = open.as_mut() {
                if let Some(piece) = pieces.next_piece()? {
                    return Ok(Some(std::borrow::Cow::Owned(piece)));
                }
                open = None;
            }
            let Some(file) = files.next() else { return Ok(None) };
            match config.stream_bytes {
                Some(size) => open = Some(Pieces::open(&file, size, line_cut)?),
                None => return Ok(Some(std::borrow::Cow::Owned(std::fs::read(&file)?))),
            }
        })
    }

    /// A tokenizer with `vocabulary` installed.
    pub fn tokenizer(&self, vocabulary: Vocabulary) -> Result<Box<dyn Tokenizer>> {
        self.registry.tokenizer_for(vocabulary)
    }

    fn adapter_of(&self, vocabulary: &Vocabulary) -> Result<(Box<dyn ModalityAdapter>, AdapterConfig)> {
        match &vocabulary.adapter {
            Some(spec) => Ok((self.registry.adapter(&spec.name)?, AdapterConfig { options: spec.options.clone() })),
            None => {
                let name = match vocabulary.manifest.payload_kind {
                    PayloadKind::Bytes => kute_adapters::BINARY,
                    PayloadKind::Symbols => kute_adapters::JSONL_EVENTS,
                };
                Ok((self.registry.adapter(name)?, AdapterConfig::default()))
            }
        }
    }

    /// Raw bytes to tokens, through the vocabulary's adapter.
    pub fn encode_raw(&self, tokenizer: &dyn Tokenizer, raw: &[u8]) -> Result<TokenSequence> {
        let vocabulary = tokenizer.vocabulary().ok_or_else(|| KuteError::NotTrained("tokenizer has no vocabulary".into()))?;
        let (adapter, config) = self.adapter_of(vocabulary)?;
        tokenizer.encode(&adapter.ingest(raw, &config)?)
    }

    /// Raw bytes to tokens under an explicit policy for unknown symbols.
    pub fn encode_raw_open(&self, tokenizer: &dyn Tokenizer, raw: &[u8], policy: UnknownPolicy) -> Result<TokenSequence> {
        let vocabulary = tokenizer.vocabulary().ok_or_else(|| KuteError::NotTrained("tokenizer has no vocabulary".into()))?;
        let (adapter, config) = self.adapter_of(vocabulary)?;
        tokenizer.encode_open(&adapter.ingest(raw, &config)?, policy)
    }

    /// Like [`Engine::decode_raw`], but refuses before decoding when the
    /// sequence would expand to more than `max_units` bytes or symbols.
    pub fn decode_raw_bounded(&self, tokenizer: &dyn Tokenizer, tokens: &TokenSequence, max_units: u64) -> Result<Vec<u8>> {
        let vocabulary = tokenizer.vocabulary().ok_or_else(|| KuteError::NotTrained("tokenizer has no vocabulary".into()))?;
        let units = kute_core::decoded_units(vocabulary, tokens)?;
        if units > max_units {
            return Err(KuteError::LimitExceeded(format!("the sequence decodes to {units} units; the budget is {max_units}")));
        }
        self.decode_raw(tokenizer, tokens)
    }

    /// Tokens back to raw bytes, through the vocabulary's adapter.
    pub fn decode_raw(&self, tokenizer: &dyn Tokenizer, tokens: &TokenSequence) -> Result<Vec<u8>> {
        let vocabulary = tokenizer.vocabulary().ok_or_else(|| KuteError::NotTrained("tokenizer has no vocabulary".into()))?;
        let (adapter, config) = self.adapter_of(vocabulary)?;
        adapter.reconstruct(&tokenizer.decode(tokens)?, &config)
    }

    /// Encode many documents in parallel. Output order matches input order.
    pub fn encode_batch(&self, tokenizer: &dyn Tokenizer, documents: &[Vec<u8>]) -> Result<Vec<TokenSequence>> {
        documents.par_iter().map(|d| self.encode_raw(tokenizer, d)).collect()
    }

    /// Run the vocabulary's embedded test vectors: each input must encode to
    /// the recorded ids and decode back to the input.
    pub fn verify(&self, vocabulary: &Vocabulary) -> Result<VerifyReport> {
        vocabulary.validate()?;
        let tokenizer = self.tokenizer(vocabulary.clone())?;
        let mut failures = Vec::new();
        let mut passed = 0;
        for (i, vector) in vocabulary.test_vectors.iter().enumerate() {
            let payload = match &vector.input {
                TokenPayload::Bytes(b) => Payload::Bytes(b.clone()),
                TokenPayload::Symbols(s) => Payload::Symbols(s.clone()),
            };
            let input = CanonicalInput { modality: vocabulary.manifest.modality, payload };
            match tokenizer.encode(&input) {
                Err(e) => failures.push(format!("vector {i}: encode failed: {e}")),
                Ok(sequence) if sequence.ids != vector.ids => failures.push(format!("vector {i}: encoded ids differ from the recorded ids")),
                Ok(sequence) => match tokenizer.decode(&sequence) {
                    Ok(back) if back.payload == input.payload => passed += 1,
                    Ok(_) => failures.push(format!("vector {i}: decode did not reproduce the input")),
                    Err(e) => failures.push(format!("vector {i}: decode failed: {e}")),
                },
            }
        }
        Ok(VerifyReport {
            vocabulary: format!("{} {}", vocabulary.id(), vocabulary.version()),
            digest: vocabulary.digest()?,
            vectors: vocabulary.test_vectors.len(),
            passed,
            failures,
        })
    }
}

/// A file read in pieces of about `size` bytes. With `line_cut`, a piece ends
/// after the last line feed within `size` bytes, or runs on to the next line
/// feed when a single line is longer than that, so no line is ever split.
struct Pieces {
    reader: std::io::BufReader<std::fs::File>,
    size: usize,
    line_cut: bool,
    carry: Vec<u8>,
    done: bool,
}

impl Pieces {
    fn open(path: &Path, size: usize, line_cut: bool) -> Result<Self> {
        Ok(Self { reader: std::io::BufReader::new(std::fs::File::open(path)?), size, line_cut, carry: Vec::new(), done: false })
    }

    fn next_piece(&mut self) -> Result<Option<Vec<u8>>> {
        use std::io::{BufRead, Read};
        if self.done && self.carry.is_empty() {
            return Ok(None);
        }
        let mut piece = std::mem::take(&mut self.carry);
        if !self.done && piece.len() < self.size {
            let want = (self.size - piece.len()) as u64;
            if (&mut self.reader).take(want).read_to_end(&mut piece)? < want as usize {
                self.done = true;
            }
        }
        if self.line_cut && !self.done {
            match piece.iter().rposition(|&b| b == b'\n') {
                Some(at) => self.carry = piece.split_off(at + 1),
                None => {
                    // One line longer than a piece: finish the line.
                    if self.reader.read_until(b'\n', &mut piece)? == 0 || piece.last() != Some(&b'\n') {
                        self.done = true;
                    }
                }
            }
        }
        Ok(if piece.is_empty() { None } else { Some(piece) })
    }
}

/// Files under `path`, recursively, in sorted order. A file path is returned
/// as is. Hidden entries (leading dot) inside directories are skipped.
pub fn collect_files(path: &Path) -> Result<Vec<PathBuf>> {
    let meta = std::fs::metadata(path).map_err(|e| KuteError::InvalidInput(format!("{}: {e}", path.display())))?;
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}
