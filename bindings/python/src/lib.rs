//! Python bindings for KUTE. See `bindings/python/README.md`.

use kute::{vocab, AdapterConfig, KuteError, Pretokenizer, Tokenizer, TrainingConfig, Version, VocabularyId};
use pyo3::exceptions::{PyOSError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn err(e: KuteError) -> PyErr {
    match e {
        KuteError::Io(e) => PyOSError::new_err(e.to_string()),
        other => PyValueError::new_err(other.to_string()),
    }
}

fn to_python(py: Python<'_>, value: &impl serde::Serialize) -> PyResult<Py<PyAny>> {
    let text = serde_json::to_string(value).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(py.import("json")?.call_method1("loads", (text,))?.unbind())
}

/// `str` or `bytes` to raw bytes.
fn raw_bytes(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if obj.is_instance_of::<PyString>() {
        return Ok(obj.extract::<String>()?.into_bytes());
    }
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    Err(PyTypeError::new_err("expected str or bytes"))
}

/// A trained vocabulary. Immutable.
#[pyclass(frozen, module = "kute")]
struct Vocabulary {
    inner: kute::Vocabulary,
    tokenizer: OnceLock<Arc<Box<dyn Tokenizer>>>,
}

impl Vocabulary {
    fn wrap(inner: kute::Vocabulary) -> Self {
        Self { inner, tokenizer: OnceLock::new() }
    }

    fn tokenizer(&self, engine: &kute::Engine) -> PyResult<Arc<Box<dyn Tokenizer>>> {
        if let Some(t) = self.tokenizer.get() {
            return Ok(t.clone());
        }
        let built = Arc::new(engine.tokenizer(self.inner.clone()).map_err(err)?);
        Ok(self.tokenizer.get_or_init(|| built).clone())
    }
}

#[pymethods]
impl Vocabulary {
    /// Read and validate a .kvf file.
    #[staticmethod]
    fn load(path: PathBuf) -> PyResult<Self> {
        Ok(Self::wrap(vocab::load(path).map_err(err)?))
    }

    /// Write a .kvf file. Refuses to replace a file with different content
    /// unless `overwrite` is true: published versions are immutable.
    #[pyo3(signature = (path, overwrite=false))]
    fn save(&self, path: PathBuf, overwrite: bool) -> PyResult<()> {
        vocab::save(&self.inner, path, overwrite).map_err(err)
    }

    #[getter]
    fn id(&self) -> String {
        self.inner.id().to_string()
    }

    #[getter]
    fn version(&self) -> String {
        self.inner.version().to_string()
    }

    #[getter]
    fn algorithm(&self) -> String {
        self.inner.manifest.algorithm.clone()
    }

    #[getter]
    fn adapter(&self) -> Option<String> {
        self.inner.adapter.as_ref().map(|a| a.name.clone())
    }

    /// Content digest, `sha256:...`.
    #[getter]
    fn digest(&self) -> PyResult<String> {
        self.inner.digest().map_err(err)
    }

    fn __len__(&self) -> usize {
        self.inner.tokens.len()
    }

    fn __repr__(&self) -> String {
        format!("<kute.Vocabulary {} {} ({}, {} tokens)>", self.inner.id(), self.inner.version(), self.inner.manifest.algorithm, self.inner.tokens.len())
    }

    /// Summary as a dict, the same data `kute inspect` prints.
    fn summary(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_python(py, &vocab::summarize(&self.inner).map_err(err)?)
    }

    /// The full envelope of one token as a dict.
    fn token(&self, py: Python<'_>, token_id: u32) -> PyResult<Py<PyAny>> {
        to_python(py, &self.inner.record(token_id).map_err(err)?)
    }

    /// Token content as bytes. Raises for symbol vocabularies.
    fn token_bytes<'py>(&self, py: Python<'py>, token_id: u32) -> PyResult<Bound<'py, PyBytes>> {
        match &self.inner.token(token_id).map_err(err)?.payload {
            kute::TokenPayload::Bytes(b) => Ok(PyBytes::new(py, b)),
            kute::TokenPayload::Symbols(_) => Err(PyValueError::new_err("this vocabulary holds symbols, not bytes; use token()")),
        }
    }

    /// Compare with another vocabulary by token content.
    fn compare(&self, py: Python<'_>, other: &Vocabulary) -> PyResult<Py<PyAny>> {
        to_python(py, &vocab::compare(&self.inner, &other.inner).map_err(err)?)
    }

    /// Write `path` in tiktoken's BPE file format and return the keyword
    /// arguments for `tiktoken.Encoding` (without `mergeable_ranks`, which
    /// `tiktoken.load.load_tiktoken_bpe(path)` provides).
    fn export_tiktoken(&self, py: Python<'_>, path: PathBuf) -> PyResult<Py<PyAny>> {
        let export = vocab::export_tiktoken(&self.inner).map_err(err)?;
        std::fs::write(&path, &export.bpe_file).map_err(|e| PyOSError::new_err(e.to_string()))?;
        let out = PyDict::new(py);
        out.set_item("name", self.inner.id().as_str().replace(':', "_"))?;
        out.set_item("pat_str", export.pat_str)?;
        out.set_item("special_tokens", export.special_tokens)?;
        out.set_item("explicit_n_vocab", export.n_vocab)?;
        Ok(out.into_any().unbind())
    }
}

/// The engine: every built-in algorithm and adapter.
#[pyclass(frozen, module = "kute")]
struct Engine {
    inner: kute::Engine,
}

#[pymethods]
impl Engine {
    #[new]
    fn new() -> Self {
        Self { inner: kute::Engine::new() }
    }

    /// Train a vocabulary.
    ///
    /// Give `source` (a path or list of paths; directories are read
    /// recursively) or `texts` (a list of str or bytes held in memory).
    ///
    /// `embed_test_vectors` stores short excerpts of the training data in
    /// the vocabulary so that `verify` has something to check; off unless
    /// asked. `resume_from` continues an earlier bpe run on the same data
    /// and settings to a larger `vocab_size`, with the result of one run.
    #[pyo3(signature = (source=None, *, texts=None, vocabulary_id, algorithm="bpe", adapter="text", vocab_size=4096,
                        version="0.1.0", pretokenizer=None, adapter_options=None, reserved_tokens=None,
                        min_frequency=1, max_token_length=None, dictionary=None, description=None, license=None,
                        embed_test_vectors=false, resume_from=None))]
    #[allow(clippy::too_many_arguments)]
    fn train(
        &self,
        py: Python<'_>,
        source: Option<&Bound<'_, PyAny>>,
        texts: Option<Vec<Bound<'_, PyAny>>>,
        vocabulary_id: &str,
        algorithm: &str,
        adapter: &str,
        vocab_size: u32,
        version: &str,
        pretokenizer: Option<&str>,
        adapter_options: Option<std::collections::BTreeMap<String, String>>,
        reserved_tokens: Option<Vec<String>>,
        min_frequency: u64,
        max_token_length: Option<usize>,
        dictionary: Option<Vec<String>>,
        description: Option<String>,
        license: Option<String>,
        embed_test_vectors: bool,
        resume_from: Option<&Vocabulary>,
    ) -> PyResult<Vocabulary> {
        let mut config = TrainingConfig::new(VocabularyId::parse(vocabulary_id).map_err(err)?, vocab_size);
        config.vocabulary_version = Version::parse(version).map_err(|e| PyValueError::new_err(format!("version {version:?}: {e}")))?;
        config.pretokenizer = match pretokenizer {
            None => {
                if adapter == "text" && algorithm != "sequence-bpe" { Pretokenizer::Gpt4 } else { Pretokenizer::None }
            }
            Some("none") => Pretokenizer::None,
            Some("gpt4") => Pretokenizer::Gpt4,
            Some(pattern) => Pretokenizer::Regex(pattern.to_string()),
        };
        config.reserved_tokens = reserved_tokens.unwrap_or_default();
        config.min_frequency = min_frequency;
        config.max_token_length = max_token_length;
        config.dictionary = dictionary.unwrap_or_default();
        config.description = description;
        config.license = license;
        config.embed_test_vectors = embed_test_vectors;
        config.resume_from = resume_from.map(|v| Box::new(v.inner.clone()));
        let adapter_config = AdapterConfig { options: adapter_options.unwrap_or_default() };

        let trained = match (source, texts) {
            (Some(_), Some(_)) => return Err(PyTypeError::new_err("give source or texts, not both")),
            (None, None) => return Err(PyTypeError::new_err("give source (paths) or texts (in-memory documents)")),
            (Some(source), None) => {
                let paths: Vec<PathBuf> = match source.extract::<PathBuf>() {
                    Ok(one) => vec![one],
                    Err(_) => source.extract::<Vec<PathBuf>>().map_err(|_| PyTypeError::new_err("source must be a path or a list of paths"))?,
                };
                py.detach(|| self.inner.train_paths(algorithm, adapter, &adapter_config, &paths, &config))
            }
            (None, Some(texts)) => {
                let documents: Vec<Vec<u8>> = texts.iter().map(raw_bytes).collect::<PyResult<_>>()?;
                py.detach(|| self.inner.train(algorithm, adapter, &adapter_config, &documents, &config))
            }
        };
        Ok(Vocabulary::wrap(trained.map_err(err)?))
    }

    /// Encode str or bytes to a list of token ids.
    #[pyo3(signature = (data, vocabulary))]
    fn encode(&self, py: Python<'_>, data: &Bound<'_, PyAny>, vocabulary: &Vocabulary) -> PyResult<Vec<u32>> {
        let raw = raw_bytes(data)?;
        let tokenizer = vocabulary.tokenizer(&self.inner)?;
        py.detach(|| self.inner.encode_raw(tokenizer.as_ref().as_ref(), &raw)).map(|s| s.ids).map_err(err)
    }

    /// Encode many documents in parallel.
    #[pyo3(signature = (documents, vocabulary))]
    fn encode_batch(&self, py: Python<'_>, documents: Vec<Bound<'_, PyAny>>, vocabulary: &Vocabulary) -> PyResult<Vec<Vec<u32>>> {
        let raw: Vec<Vec<u8>> = documents.iter().map(raw_bytes).collect::<PyResult<_>>()?;
        let tokenizer = vocabulary.tokenizer(&self.inner)?;
        let out = py.detach(|| self.inner.encode_batch(tokenizer.as_ref().as_ref(), &raw)).map_err(err)?;
        Ok(out.into_iter().map(|s| s.ids).collect())
    }

    /// Decode token ids. Returns str for vocabularies trained through the
    /// text adapter, bytes otherwise.
    #[pyo3(signature = (tokens, vocabulary))]
    fn decode(&self, py: Python<'_>, tokens: Vec<u32>, vocabulary: &Vocabulary) -> PyResult<Py<PyAny>> {
        let bytes = self.decode_raw(py, tokens, vocabulary)?;
        if vocabulary.inner.adapter.as_ref().map(|a| a.name.as_str()) == Some("text") {
            let text = String::from_utf8(bytes)
                .map_err(|_| PyValueError::new_err("these tokens do not decode to valid UTF-8; use decode_bytes"))?;
            Ok(PyString::new(py, &text).into_any().unbind())
        } else {
            Ok(PyBytes::new(py, &bytes).into_any().unbind())
        }
    }

    /// Decode token ids to bytes.
    #[pyo3(signature = (tokens, vocabulary))]
    fn decode_bytes<'py>(&self, py: Python<'py>, tokens: Vec<u32>, vocabulary: &Vocabulary) -> PyResult<Bound<'py, PyBytes>> {
        Ok(PyBytes::new(py, &self.decode_raw(py, tokens, vocabulary)?))
    }

    /// Measure a vocabulary on a dataset (file or directory). Returns a dict.
    #[pyo3(signature = (vocabulary, dataset, iterations=3))]
    fn evaluate(&self, py: Python<'_>, vocabulary: &Vocabulary, dataset: PathBuf, iterations: usize) -> PyResult<Py<PyAny>> {
        let report = py
            .detach(|| {
                let documents = kute_bench::read_dataset(&dataset)?;
                kute_bench::evaluate(&self.inner, &vocabulary.inner, &documents, iterations)
            })
            .map_err(err)?;
        to_python(py, &report)
    }

    /// Run the vocabulary's embedded test vectors. Returns a dict.
    fn verify(&self, py: Python<'_>, vocabulary: &Vocabulary) -> PyResult<Py<PyAny>> {
        to_python(py, &self.inner.verify(&vocabulary.inner).map_err(err)?)
    }

    /// Names of the registered algorithms.
    fn algorithms(&self) -> Vec<String> {
        self.inner.registry().algorithm_names()
    }

    /// Names of the registered adapters.
    fn adapters(&self) -> Vec<String> {
        self.inner.registry().adapter_names()
    }
}

impl Engine {
    fn decode_raw(&self, py: Python<'_>, tokens: Vec<u32>, vocabulary: &Vocabulary) -> PyResult<Vec<u8>> {
        let tokenizer = vocabulary.tokenizer(&self.inner)?;
        let v = &vocabulary.inner;
        let sequence = kute::TokenSequence {
            vocabulary_id: v.id().clone(),
            vocabulary_version: v.version().clone(),
            vocabulary_digest: v.digest().map_err(err)?,
            modality: v.manifest.modality,
            adapter: v.adapter.as_ref().map(|a| a.name.clone()),
            ids: tokens,
            escapes: Vec::new(),
        };
        py.detach(|| self.inner.decode_raw(tokenizer.as_ref().as_ref(), &sequence)).map_err(err)
    }
}

/// Find recurring patterns in records of symbols, with no tokenizer.
///
/// `records` is a list of lists of str. Returns a dict: `patterns` (runs of
/// adjacent symbols and gapped pairs, strongest first, each with its
/// support and evidence), `observation` and the `params` used. Patterns are
/// statistical regularities in order; nothing is said about what they mean.
#[pyfunction]
#[pyo3(signature = (records, *, min_support=None, max_length=None, window=None, max_patterns=None))]
fn discover(
    py: Python<'_>,
    records: Vec<Vec<String>>,
    min_support: Option<u64>,
    max_length: Option<usize>,
    window: Option<usize>,
    max_patterns: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let mut params = kute_discover::Params::default();
    if let Some(v) = min_support {
        params.min_support = v;
    }
    if let Some(v) = max_length {
        params.max_length = v;
    }
    if let Some(v) = window {
        params.window = v;
    }
    if let Some(v) = max_patterns {
        params.max_patterns = v;
    }
    let found = py.detach(|| kute_discover::discover(&records, &params)).map_err(err)?;
    to_python(py, &found)
}

#[pymodule(name = "kute")]
fn kute_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_class::<Vocabulary>()?;
    m.add_function(wrap_pyfunction!(discover, m)?)?;
    m.add("__version__", kute::KUTE_VERSION)?;
    m.add("KVF_VERSION", kute::KVF_VERSION)?;
    Ok(())
}
