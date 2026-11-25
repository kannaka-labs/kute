//! Core interfaces of the Kannaka Universal Tokenization Engine.
//!
//! Nothing here knows about a particular algorithm or data format. Algorithms
//! implement [`Tokenizer`], formats implement [`ModalityAdapter`], and both are
//! added to a [`Registry`] at run time, so third parties extend KUTE without
//! changing this crate (FR-002, ADR-0003).

use kute_types::{
    AdapterConfig, AdapterSpec, CanonicalInput, KuteError, Modality, Payload, Result, TokenPayload, TokenSequence,
    TokenizerCapabilities, TrainingConfig, UnknownPolicy, Vocabulary,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

pub use kute_types as types;

/// A stream of canonical records to train on.
pub trait CorpusSource {
    /// The next record, or `None` when the corpus is exhausted.
    fn next_record(&mut self) -> Result<Option<CanonicalInput>>;
}

/// An in-memory corpus.
pub struct VecCorpus {
    records: std::vec::IntoIter<CanonicalInput>,
}

impl VecCorpus {
    pub fn new(records: Vec<CanonicalInput>) -> Self {
        Self { records: records.into_iter() }
    }
}

impl CorpusSource for VecCorpus {
    fn next_record(&mut self) -> Result<Option<CanonicalInput>> {
        Ok(self.records.next())
    }
}

/// A corpus backed by a closure, for streaming sources.
pub struct FnCorpus<F: FnMut() -> Result<Option<CanonicalInput>>>(pub F);

impl<F: FnMut() -> Result<Option<CanonicalInput>>> CorpusSource for FnCorpus<F> {
    fn next_record(&mut self) -> Result<Option<CanonicalInput>> {
        (self.0)()
    }
}

/// The generic tokenizer interface (PRD 9.3).
///
/// Two deliberate differences from the PRD sketch: `vocabulary` returns an
/// `Option` because a tokenizer exists before it is trained, and `load` installs
/// an existing vocabulary. Both are recorded in ADR-0003.
pub trait Tokenizer: Send + Sync {
    /// Learn a vocabulary from `corpus`, install it, and return a copy.
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary>;

    /// Install a vocabulary produced earlier. Implementations validate it.
    fn load(&mut self, vocabulary: Vocabulary) -> Result<()>;

    fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence>;

    /// Encode under an explicit policy for symbols the vocabulary lacks.
    ///
    /// `Strict` is plain [`Tokenizer::encode`]. `Escape` must return a
    /// sequence that decodes to the input exactly, without changing the
    /// vocabulary. Algorithms that can meet no unknown symbol (byte-level
    /// ones) need not override this; algorithms that can, and have no
    /// escape support, report that instead of guessing.
    fn encode_open(&self, input: &CanonicalInput, policy: UnknownPolicy) -> Result<TokenSequence> {
        match (policy, self.encode(input)) {
            (UnknownPolicy::Escape, Err(KuteError::UnknownSymbol(symbol))) => Err(KuteError::Unsupported(format!(
                "symbol {symbol:?} is not in the vocabulary and the {} algorithm has no escape encoding",
                self.capabilities().algorithm
            ))),
            (_, result) => result,
        }
    }

    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput>;

    fn vocabulary(&self) -> Option<&Vocabulary>;

    fn capabilities(&self) -> TokenizerCapabilities;
}

/// What an adapter declares about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub name: String,
    pub modality: Modality,
    /// `reconstruct(ingest(x)) == x` byte for byte, for every accepted `x`.
    pub lossless: bool,
    pub notes: String,
}

/// Converts raw bytes of some format to canonical input and back (PRD 9.3).
pub trait ModalityAdapter: Send + Sync {
    fn modality(&self) -> Modality;

    fn ingest(&self, input: &[u8], config: &AdapterConfig) -> Result<CanonicalInput>;

    /// `config` is the configuration the input was ingested with; a
    /// vocabulary stores it, so decoding uses the same field names and options.
    fn reconstruct(&self, input: &CanonicalInput, config: &AdapterConfig) -> Result<Vec<u8>>;

    /// Must state whether reconstruction is lossless.
    fn capabilities(&self) -> AdapterCapabilities;

    /// The specification stored in a vocabulary trained through this adapter.
    fn spec(&self, config: &AdapterConfig) -> AdapterSpec {
        let caps = self.capabilities();
        AdapterSpec { name: caps.name, options: config.options.clone(), lossless: caps.lossless }
    }
}

/// A numeric representation of one token (FR-015).
#[derive(Clone, Debug, PartialEq)]
pub enum Representation {
    /// Real-valued vector.
    Vector(Vec<f32>),
    /// Complex-valued vector as `(re, im)` pairs.
    ComplexVector(Vec<(f32, f32)>),
}

/// Maps token ids to another representation. Mappings are versioned on their
/// own and never change what a token id means (FR-015, ADR-0011).
pub trait RepresentationBackend: Send + Sync {
    fn mapping_id(&self) -> &str;
    fn mapping_version(&self) -> &str;
    fn dimension(&self) -> usize;
    fn map(&self, vocabulary: &Vocabulary, token_id: u32) -> Result<Representation>;
}

type TokenizerFactory = Arc<dyn Fn() -> Box<dyn Tokenizer> + Send + Sync>;
type AdapterFactory = Arc<dyn Fn() -> Box<dyn ModalityAdapter> + Send + Sync>;

/// Run-time table of algorithms and adapters.
#[derive(Clone, Default)]
pub struct Registry {
    algorithms: BTreeMap<String, TokenizerFactory>,
    adapters: BTreeMap<String, AdapterFactory>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tokenization algorithm under `name`. A name can be taken
    /// once: a second registration is an error, so one plugin cannot quietly
    /// change what another's vocabularies mean. Use
    /// [`Registry::replace_algorithm`] to override on purpose.
    pub fn register_algorithm<F>(&mut self, name: &str, factory: F) -> Result<()>
    where
        F: Fn() -> Box<dyn Tokenizer> + Send + Sync + 'static,
    {
        if self.algorithms.contains_key(name) {
            return Err(KuteError::InvalidConfig(format!("algorithm {name:?} is already registered")));
        }
        self.algorithms.insert(name.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Register or replace the algorithm under `name`.
    pub fn replace_algorithm<F>(&mut self, name: &str, factory: F)
    where
        F: Fn() -> Box<dyn Tokenizer> + Send + Sync + 'static,
    {
        self.algorithms.insert(name.to_string(), Arc::new(factory));
    }

    /// Register an adapter under `name`. Like algorithms, a name can be
    /// taken once.
    pub fn register_adapter<F>(&mut self, name: &str, factory: F) -> Result<()>
    where
        F: Fn() -> Box<dyn ModalityAdapter> + Send + Sync + 'static,
    {
        if self.adapters.contains_key(name) {
            return Err(KuteError::InvalidConfig(format!("adapter {name:?} is already registered")));
        }
        self.adapters.insert(name.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Register or replace the adapter under `name`.
    pub fn replace_adapter<F>(&mut self, name: &str, factory: F)
    where
        F: Fn() -> Box<dyn ModalityAdapter> + Send + Sync + 'static,
    {
        self.adapters.insert(name.to_string(), Arc::new(factory));
    }

    /// A fresh, untrained tokenizer for `name`.
    pub fn tokenizer(&self, name: &str) -> Result<Box<dyn Tokenizer>> {
        self.algorithms.get(name).map(|f| f()).ok_or_else(|| {
            KuteError::Unsupported(format!(
                "algorithm {name:?} is not registered (available: {})",
                self.algorithm_names().join(", ")
            ))
        })
    }

    pub fn adapter(&self, name: &str) -> Result<Box<dyn ModalityAdapter>> {
        self.adapters.get(name).map(|f| f()).ok_or_else(|| {
            KuteError::Unsupported(format!(
                "adapter {name:?} is not registered (available: {})",
                self.adapter_names().join(", ")
            ))
        })
    }

    /// A tokenizer with `vocabulary` installed, chosen by the manifest.
    /// A tokenizer with `vocabulary` installed.
    ///
    /// This is the sealed boundary of the runtime. A `Vocabulary` value is
    /// plain data with public fields, and its `digest()` reports what was
    /// recorded; code may have changed the content since. What a tokenizer
    /// returned from here holds cannot be changed (it hands out only shared
    /// references), and it is checked here, whatever the algorithm's own
    /// `load` did: it carries a digest, and that digest is the hash of its
    /// content. So `tokenizer.vocabulary().digest()` always names the
    /// vocabulary that is doing the encoding.
    pub fn tokenizer_for(&self, vocabulary: Vocabulary) -> Result<Box<dyn Tokenizer>> {
        let mut t = self.tokenizer(&vocabulary.manifest.algorithm)?;
        t.load(vocabulary)?;
        let held = t.vocabulary().ok_or_else(|| KuteError::InvalidVocabulary("the tokenizer holds no vocabulary after loading one".into()))?;
        let Some(integrity) = &held.integrity else {
            return Err(KuteError::Integrity("the tokenizer loaded a vocabulary without recording its digest".into()));
        };
        let actual = held.compute_digest()?;
        if actual != integrity.digest {
            return Err(KuteError::Integrity(format!("recorded digest {} does not match content digest {actual}", integrity.digest)));
        }
        Ok(t)
    }

    pub fn algorithm_names(&self) -> Vec<String> {
        self.algorithms.keys().cloned().collect()
    }

    pub fn adapter_names(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }
}

/// Order-sensitive fingerprint of a corpus as a tokenizer sees it. Trainers
/// feed every record through this and store the result as provenance.
pub struct CorpusFingerprint {
    hasher: Sha256,
    records: u64,
    units: u64,
}

impl Default for CorpusFingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl CorpusFingerprint {
    pub fn new() -> Self {
        Self { hasher: Sha256::new(), records: 0, units: 0 }
    }

    pub fn update(&mut self, input: &CanonicalInput) {
        match &input.payload {
            Payload::Bytes(b) => {
                self.hasher.update(b"B");
                self.hasher.update((b.len() as u64).to_le_bytes());
                self.hasher.update(b);
            }
            Payload::Symbols(s) => {
                self.hasher.update(b"S");
                self.hasher.update((s.len() as u64).to_le_bytes());
                for sym in s {
                    self.hasher.update((sym.len() as u64).to_le_bytes());
                    self.hasher.update(sym.as_bytes());
                }
            }
        }
        self.records += 1;
        self.units += input.payload.len() as u64;
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    /// Total bytes or symbols seen.
    pub fn units(&self) -> u64 {
        self.units
    }

    pub fn finish(self) -> String {
        format!("sha256:{}", kute_types::hex(&self.hasher.finalize()))
    }
}

/// Check that a token sequence was produced by exactly this vocabulary.
pub fn check_sequence(vocabulary: &Vocabulary, tokens: &TokenSequence) -> Result<()> {
    if &tokens.vocabulary_id != vocabulary.id() || &tokens.vocabulary_version != vocabulary.version() {
        return Err(KuteError::VocabularyMismatch(format!(
            "sequence was encoded with {} {} but the loaded vocabulary is {} {}",
            tokens.vocabulary_id,
            tokens.vocabulary_version,
            vocabulary.id(),
            vocabulary.version()
        )));
    }
    let digest = vocabulary.digest()?;
    if tokens.vocabulary_digest != digest {
        return Err(KuteError::VocabularyMismatch(format!(
            "same id and version but different content: sequence has {}, vocabulary has {digest}",
            tokens.vocabulary_digest
        )));
    }
    Ok(())
}

/// Build the sequence header for `vocabulary`.
pub fn sequence_for(vocabulary: &Vocabulary, modality: Modality, ids: Vec<u32>) -> Result<TokenSequence> {
    Ok(TokenSequence {
        vocabulary_id: vocabulary.id().clone(),
        vocabulary_version: vocabulary.version().clone(),
        vocabulary_digest: vocabulary.digest()?,
        modality,
        adapter: vocabulary.adapter.as_ref().map(|a| a.name.clone()),
        ids,
        escapes: Vec::new(),
    })
}

/// How many units (bytes or symbols) `tokens` decodes to, computed from the
/// token table without decoding. A five-byte id can stand for a 64 KiB
/// token, so callers that decode untrusted sequences check this against a
/// budget first.
pub fn decoded_units(vocabulary: &Vocabulary, tokens: &TokenSequence) -> Result<u64> {
    let table = vocabulary.tokens.len();
    let mut total: u64 = 0;
    for &id in &tokens.ids {
        let id = id as usize;
        let units = if id < table {
            vocabulary.tokens[id].payload.len() as u64
        } else if id - table < tokens.escapes.len() {
            match vocabulary.tokens[0].payload {
                TokenPayload::Bytes(_) => tokens.escapes[id - table].len() as u64,
                TokenPayload::Symbols(_) => 1,
            }
        } else {
            return Err(KuteError::UnknownToken(id as u32));
        };
        total = total.saturating_add(units);
    }
    Ok(total)
}

/// A lower bound on the bytes `tokens` decodes to. For a byte vocabulary it
/// is the decoded length. For a symbol vocabulary it is the total length of
/// the symbol names, before the adapter writes them out, which only adds to
/// it. A symbol may be up to 1024 bytes long, so the number of symbols
/// ([`decoded_units`]) says little about the bytes; this does.
pub fn decoded_bytes_at_least(vocabulary: &Vocabulary, tokens: &TokenSequence) -> Result<u64> {
    let size = |payload: &TokenPayload| match payload {
        TokenPayload::Bytes(b) => b.len() as u64,
        TokenPayload::Symbols(s) => s.iter().map(|symbol| symbol.len() as u64).sum(),
    };
    let sizes: Vec<u64> = vocabulary.tokens.iter().map(|t| size(&t.payload)).collect();
    let mut total: u64 = 0;
    for &id in &tokens.ids {
        let id = id as usize;
        let bytes = if id < sizes.len() {
            sizes[id]
        } else if id - sizes.len() < tokens.escapes.len() {
            tokens.escapes[id - sizes.len()].len() as u64
        } else {
            return Err(KuteError::UnknownToken(id as u32));
        };
        total = total.saturating_add(bytes);
    }
    Ok(total)
}
