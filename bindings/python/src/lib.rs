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
    /// The next version of an event vocabulary (`sequence-bpe`): each
    /// addition is a list of symbol names, one name for a new symbol or
    /// several for a sequence. Every existing token keeps its id. The
    /// child is checked against its test vectors before it is returned.
    /// Whether the child is worth using is a separate question:
    /// measure it (`kute shadow`).
    #[pyo3(signature = (additions, *, version))]
    fn evolve(&self, py: Python<'_>, additions: Vec<Vec<String>>, version: &str) -> PyResult<Self> {
        let version = Version::parse(version).map_err(|e| PyValueError::new_err(format!("version {version:?}: {e}")))?;
        let child = py.detach(|| kute::bpe::sequence::evolve(&self.inner, &additions, version)).map_err(err)?;
        let report = kute::Engine::new().verify(&child).map_err(err)?;
        if !report.ok() {
            return Err(PyValueError::new_err(format!("the evolved vocabulary fails its test vectors: {}", report.failures.join("; "))));
        }
        Ok(Self::wrap(child))
    }

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

    /// Encode into a wire container: `bytes` that name the vocabulary by
    /// its digest and can be given to another process or kept in a file.
    ///
    /// `unknown="escape"` carries a symbol the vocabulary lacks literally
    /// in the container, so nothing is lost; the default, `"strict"`,
    /// refuses the input. `compact=False` writes the plain form.
    #[pyo3(signature = (data, vocabulary, *, unknown="strict", compact=true))]
    fn pack<'py>(&self, py: Python<'py>, data: &Bound<'py, PyAny>, vocabulary: &Vocabulary, unknown: &str, compact: bool) -> PyResult<Bound<'py, PyBytes>> {
        let raw = raw_bytes(data)?;
        let policy = kute::UnknownPolicy::parse(unknown).map_err(err)?;
        let tokenizer = vocabulary.tokenizer(&self.inner)?;
        let bytes = py
            .detach(|| {
                let sequence = self.inner.encode_raw_open(tokenizer.as_ref().as_ref(), &raw, policy)?;
                if compact { sequence.to_bytes_compressed() } else { sequence.to_bytes() }
            })
            .map_err(err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Decode a wire container to `bytes`. The container must name this
    /// vocabulary. It is refused before decoding when it would come to
    /// more than `max_units` bytes or symbols, or more than `max_bytes`.
    #[pyo3(signature = (container, vocabulary, *, max_units=1 << 28, max_bytes=1 << 30))]
    fn unpack<'py>(&self, py: Python<'py>, container: &[u8], vocabulary: &Vocabulary, max_units: u64, max_bytes: u64) -> PyResult<Bound<'py, PyBytes>> {
        let tokenizer = vocabulary.tokenizer(&self.inner)?;
        let bytes = py
            .detach(|| {
                let sequence = kute::TokenSequence::from_bytes_bounded(container, max_units)?;
                self.inner.decode_raw_within(tokenizer.as_ref().as_ref(), &sequence, max_units, max_bytes)
            })
            .map_err(err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// What a wire container says about itself, without a vocabulary:
    /// the vocabulary it names, how many tokens and escaped symbols.
    #[staticmethod]
    #[pyo3(signature = (container, *, max_tokens=1 << 28))]
    fn inspect_container(py: Python<'_>, container: &[u8], max_tokens: u64) -> PyResult<Py<PyAny>> {
        let s = kute::TokenSequence::from_bytes_bounded(container, max_tokens).map_err(err)?;
        to_python(
            py,
            &serde_json::json!({
                "vocabulary_id": s.vocabulary_id,
                "vocabulary_version": s.vocabulary_version,
                "vocabulary_digest": s.vocabulary_digest,
                "modality": s.modality,
                "adapter": s.adapter,
                "tokens": s.ids.len(),
                "escaped_symbols": s.escapes.len(),
                "bytes": container.len(),
            }),
        )
    }

    /// Re-encode a wire container under another vocabulary. Symbols the
    /// target lacks are escaped, so the result decodes to what the
    /// original decodes to. The same limits as `unpack`.
    #[pyo3(signature = (container, source, target, *, max_units=1 << 28, max_bytes=1 << 30))]
    fn translate<'py>(&self, py: Python<'py>, container: &[u8], source: &Vocabulary, target: &Vocabulary, max_units: u64, max_bytes: u64) -> PyResult<Bound<'py, PyBytes>> {
        let from = source.tokenizer(&self.inner)?;
        let to = target.tokenizer(&self.inner)?;
        let bytes = py
            .detach(|| {
                let sequence = kute::TokenSequence::from_bytes_bounded(container, max_units)?;
                let raw = self.inner.decode_raw_within(from.as_ref().as_ref(), &sequence, max_units, max_bytes)?;
                self.inner.encode_raw_open(to.as_ref().as_ref(), &raw, kute::UnknownPolicy::Escape)?.to_bytes_compressed()
            })
            .map_err(err)?;
        Ok(PyBytes::new(py, &bytes))
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
///
/// `times`, a list of lists of int aligned with `records` and never going
/// back within a record, adds `timing`: for each pattern, how long its
/// occurrences take, in the ticks given. `held_out`, more records that
/// were not searched, adds `held_out`: how often each pattern occurs
/// there and whether it is confirmed. `held_out_times` adds
/// `held_out_timing`.
#[pyfunction]
#[pyo3(signature = (records, *, min_support=None, max_length=None, window=None, max_patterns=None, times=None, held_out=None, held_out_times=None))]
#[allow(clippy::too_many_arguments)]
fn discover(
    py: Python<'_>,
    records: Vec<Vec<String>>,
    min_support: Option<u64>,
    max_length: Option<usize>,
    window: Option<usize>,
    max_patterns: Option<usize>,
    times: Option<Vec<Vec<i64>>>,
    held_out: Option<Vec<Vec<String>>>,
    held_out_times: Option<Vec<Vec<i64>>>,
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
    if held_out_times.is_some() && held_out.is_none() {
        return Err(PyValueError::new_err("held_out_times needs held_out"));
    }
    let answer = py
        .detach(|| -> kute::Result<serde_json::Value> {
            let found = kute_discover::discover(&records, &params)?;
            let mut answer = serde_json::to_value(&found).map_err(|e| kute::KuteError::InvalidInput(e.to_string()))?;
            let json = |v: &dyn erased::Json| v.json();
            if let Some(held) = &held_out {
                answer["held_out"] = json(&kute_discover::evaluate(&found, held)?)?;
            }
            if let Some(times) = times {
                answer["timing"] = json(&kute_discover::timing(&found, &timed(records, times, "times")?)?)?;
            }
            if let (Some(held), Some(times)) = (held_out, held_out_times) {
                answer["held_out_timing"] = json(&kute_discover::timing(&found, &timed(held, times, "held_out_times")?)?)?;
            }
            Ok(answer)
        })
        .map_err(err)?;
    to_python(py, &answer)
}

mod erased {
    /// Anything serializable, to a JSON value, with KUTE's error.
    pub trait Json {
        fn json(&self) -> kute::Result<serde_json::Value>;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> kute::Result<serde_json::Value> {
            serde_json::to_value(self).map_err(|e| kute::KuteError::InvalidInput(e.to_string()))
        }
    }
}

/// Records and their times as a timed event stream. The unit is the
/// caller's: durations come back in the ticks that were given. Times not
/// aligned with their symbols, or going back, are refused.
fn timed(records: Vec<Vec<String>>, times: Vec<Vec<i64>>, name: &str) -> kute::Result<kute::EventStream> {
    if times.len() != records.len() {
        return Err(kute::KuteError::InvalidInput(format!("{name}: {} lists of times for {} records", times.len(), records.len())));
    }
    let schema = kute::EventSchema {
        id: "call".into(),
        version: 1,
        symbol_field: "symbol".into(),
        time_field: Some("time".into()),
        time_unit: Some(kute::TimeUnit::Milliseconds),
        group_field: None,
    };
    let mut stream = kute::EventStream::new(schema)?;
    stream.records = records.into_iter().zip(times).map(|(symbols, times)| kute::EventRecord { key: None, symbols, times }).collect();
    stream.validate()?;
    Ok(stream)
}

fn events_codec(name: Option<&str>) -> PyResult<kute_events::Codec> {
    name.map_or(Ok(kute_events::Codec::preferred()), kute_events::Codec::parse).map_err(err)
}

/// Pack JSON Lines of whole event records into a `KUTE-EV1` container.
///
/// `source` is `str` or `bytes`, one JSON object a line. `symbol_field`
/// names the event; `time_field` (a whole number) and `group_field` (the
/// entity) are optional. Returns `(container, report)`: the container as
/// `bytes`, and a dict with the header and the sizes. Every field of
/// every record is kept; `events_unpack` gives them back as canonical
/// JSON in source order.
#[pyfunction]
#[pyo3(signature = (source, *, symbol_field, time_field=None, group_field=None, codec=None))]
fn events_pack(
    py: Python<'_>,
    source: &Bound<'_, PyAny>,
    symbol_field: String,
    time_field: Option<String>,
    group_field: Option<String>,
    codec: Option<&str>,
) -> PyResult<(Py<PyBytes>, Py<PyAny>)> {
    let source = raw_bytes(source)?;
    let layout = kute_events::Layout { symbol_field, time_field, group_field };
    let codec = events_codec(codec)?;
    let (container, report) = py.detach(|| kute_events::pack_with(&source, &layout, codec)).map_err(err)?;
    Ok((PyBytes::new(py, &container).unbind(), to_python(py, &report)?))
}

/// The records of a `KUTE-EV1` container, as canonical JSON Lines.
/// `max_bytes` bounds what a container may ask this process to hold.
#[pyfunction]
#[pyo3(signature = (container, *, max_bytes=1 << 30))]
fn events_unpack(py: Python<'_>, container: &[u8], max_bytes: u64) -> PyResult<Py<PyBytes>> {
    let records = py.detach(|| kute_events::unpack(container, max_bytes)).map_err(err)?;
    Ok(PyBytes::new(py, &records).unbind())
}

/// The header of a `KUTE-EV1` container, without unpacking it.
#[pyfunction]
fn events_inspect(py: Python<'_>, container: &[u8]) -> PyResult<Py<PyAny>> {
    to_python(py, &kute_events::header(container).map_err(err)?.0)
}

/// Pack a sequence of numbers into a `KUTE-SG1` container.
///
/// Without `max_error` every value comes back with the same bits. With
/// it, every finite value comes back within that absolute error, checked
/// on each value before the container is returned; values that are not
/// finite still come back exactly. Returns `(container, report)`.
#[pyfunction]
#[pyo3(signature = (values, *, max_error=None, codec=None))]
fn signals_pack(py: Python<'_>, values: Vec<f64>, max_error: Option<f64>, codec: Option<&str>) -> PyResult<(Py<PyBytes>, Py<PyAny>)> {
    let mode = max_error.map_or(kute_signals::Mode::Exact, |max_error| kute_signals::Mode::Bounded { max_error });
    let codec = codec.map_or(Ok(kute_signals::Codec::preferred()), kute_signals::Codec::parse).map_err(err)?;
    let (container, report) = py.detach(|| kute_signals::pack_with(&values, mode, codec)).map_err(err)?;
    Ok((PyBytes::new(py, &container).unbind(), to_python(py, &report)?))
}

/// The values of a `KUTE-SG1` container, as a list of float. A
/// container that would give other values than its header promises is
/// refused.
#[pyfunction]
// The same default as `kute signals unpack`: about 740 MB at the most.
#[pyo3(signature = (container, *, max_values=1 << 24))]
fn signals_unpack(py: Python<'_>, container: &[u8], max_values: u64) -> PyResult<Vec<f64>> {
    py.detach(|| kute_signals::unpack(container, max_values)).map_err(err)
}

/// The header of a `KUTE-SG1` container: the contract, the bound, the
/// largest error incurred and the hash of the values.
#[pyfunction]
fn signals_inspect(py: Python<'_>, container: &[u8]) -> PyResult<Py<PyAny>> {
    to_python(py, &kute_signals::header(container).map_err(err)?)
}

#[pymodule(name = "kute")]
fn kute_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_class::<Vocabulary>()?;
    m.add_function(wrap_pyfunction!(discover, m)?)?;
    m.add_function(wrap_pyfunction!(events_pack, m)?)?;
    m.add_function(wrap_pyfunction!(events_unpack, m)?)?;
    m.add_function(wrap_pyfunction!(events_inspect, m)?)?;
    m.add_function(wrap_pyfunction!(signals_pack, m)?)?;
    m.add_function(wrap_pyfunction!(signals_unpack, m)?)?;
    m.add_function(wrap_pyfunction!(signals_inspect, m)?)?;
    m.add("ZSTD", kute_events::Codec::Zstd.available())?;
    m.add("__version__", kute::KUTE_VERSION)?;
    m.add("KVF_VERSION", kute::KVF_VERSION)?;
    Ok(())
}
