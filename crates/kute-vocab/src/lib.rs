//! KUTE Vocabulary Format (KVF) 0.1: files, validation, comparison, export.
//!
//! The format is specified in `specifications/kvf/KVF-0.1.md`. A `.kvf` file is
//! one UTF-8 JSON document. Loading always validates structure and, when the
//! file carries a digest, integrity; nothing in a file is trusted before that.

use kute_types::{b64, KuteError, PayloadKind, Result, TokenKind, TokenPayload, Vocabulary};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Largest `.kvf` file this build will read.
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// Parse and validate a KVF document.
pub fn from_slice(bytes: &[u8]) -> Result<Vocabulary> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(KuteError::LimitExceeded("vocabulary file is larger than 512 MiB".into()));
    }
    let vocabulary: Vocabulary =
        serde_json::from_slice(bytes).map_err(|e| KuteError::InvalidVocabulary(format!("not a KVF document: {e}")))?;
    vocabulary.validate()?;
    Ok(vocabulary)
}

/// Read and validate a `.kvf` file.
pub fn load(path: impl AsRef<Path>) -> Result<Vocabulary> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE_BYTES {
        return Err(KuteError::LimitExceeded(format!("{} is larger than 512 MiB", path.display())));
    }
    from_slice(&std::fs::read(path)?)
}

/// Serialize to KVF text. The vocabulary is validated and sealed first, so a
/// file written here always carries a correct digest.
///
/// Layout is for people and diffs: sections in a fixed order, one token, merge
/// rule or test vector per line. Layout carries no meaning; the digest is
/// computed over the parsed model, not over these bytes.
pub fn to_vec(vocabulary: &Vocabulary) -> Result<Vec<u8>> {
    let mut sealed = vocabulary.clone();
    sealed.seal()?;
    sealed.validate()?;
    let value = serde_json::to_value(&sealed)?;
    let serde_json::Value::Object(map) = value else {
        return Err(KuteError::Serde("vocabulary did not serialize to an object".into()));
    };
    const ORDER: &[&str] = &[
        "manifest", "pretokenizer", "adapter", "statistics", "provenance", "representations", "tokens", "merges",
        "test_vectors", "integrity",
    ];
    const ROWS: &[&str] = &["tokens", "merges", "test_vectors"];
    let mut out = String::from("{\n");
    let mut first = true;
    for key in ORDER {
        let Some(section) = map.get(*key) else { continue };
        if !first {
            out.push_str(",\n");
        }
        first = false;
        out.push_str(&format!("{}: ", serde_json::to_string(key)?));
        match section {
            serde_json::Value::Array(rows) if ROWS.contains(key) => {
                if rows.is_empty() {
                    out.push_str("[]");
                } else {
                    out.push_str("[\n");
                    for (i, row) in rows.iter().enumerate() {
                        out.push_str(&serde_json::to_string(row)?);
                        out.push_str(if i + 1 < rows.len() { ",\n" } else { "\n" });
                    }
                    out.push(']');
                }
            }
            other => out.push_str(&serde_json::to_string_pretty(other)?),
        }
    }
    out.push_str("\n}\n");
    if map.keys().any(|k| !ORDER.contains(&k.as_str())) {
        return Err(KuteError::Serde("vocabulary has a section the KVF writer does not know".into()));
    }
    Ok(out.into_bytes())
}

/// Write a `.kvf` file. Refuses to overwrite a published file with different
/// content: published versions are immutable (FR-007, ADR-0008).
pub fn save(vocabulary: &Vocabulary, path: impl AsRef<Path>, overwrite: bool) -> Result<()> {
    let path = path.as_ref();
    let bytes = to_vec(vocabulary)?;
    if path.exists() && !overwrite {
        let same = load(path).ok().and_then(|old| old.digest().ok()) == vocabulary.compute_digest().ok();
        if !same {
            return Err(KuteError::InvalidInput(format!(
                "{} already exists with different content; vocabulary versions are immutable, so bump the version or pass overwrite",
                path.display()
            )));
        }
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Summary used by `kute inspect`.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub id: String,
    pub version: String,
    pub kvf_version: String,
    pub algorithm: String,
    pub modality: kute_types::Modality,
    pub payload_kind: PayloadKind,
    pub lossless: bool,
    pub adapter: Option<String>,
    pub adapter_lossless: Option<bool>,
    pub pretokenizer: Option<String>,
    pub digest: String,
    pub tokens: usize,
    pub tokens_by_kind: BTreeMap<String, usize>,
    pub merges: usize,
    pub longest_token_units: usize,
    pub test_vectors: usize,
    pub statistics: BTreeMap<String, u64>,
    pub corpus_fingerprints: Vec<String>,
    pub license: Option<String>,
    pub generator: String,
}

pub fn summarize(v: &Vocabulary) -> Result<Summary> {
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for t in &v.tokens {
        let name = serde_json::to_value(t.kind)?.as_str().unwrap_or("unknown").to_string();
        *by_kind.entry(name).or_insert(0) += 1;
    }
    Ok(Summary {
        id: v.id().to_string(),
        version: v.version().to_string(),
        kvf_version: v.manifest.kvf_version.clone(),
        algorithm: v.manifest.algorithm.clone(),
        modality: v.manifest.modality,
        payload_kind: v.manifest.payload_kind,
        lossless: v.manifest.lossless,
        adapter: v.adapter.as_ref().map(|a| a.name.clone()),
        adapter_lossless: v.adapter.as_ref().map(|a| a.lossless),
        pretokenizer: v.pretokenizer.clone(),
        digest: v.digest()?,
        tokens: v.tokens.len(),
        tokens_by_kind: by_kind,
        merges: v.merges.len(),
        longest_token_units: v.tokens.iter().map(|t| t.payload.len()).max().unwrap_or(0),
        test_vectors: v.test_vectors.len(),
        statistics: v.statistics.clone(),
        corpus_fingerprints: v.provenance.corpus_fingerprints.clone(),
        license: v.provenance.license.clone(),
        generator: v.manifest.generator.clone(),
    })
}

/// How two vocabularies relate. Comparison is by token payload, never by id:
/// the same number in two vocabularies means nothing (FR-005).
#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
    pub a: String,
    pub b: String,
    pub identical_content: bool,
    pub same_algorithm: bool,
    pub same_payload_kind: bool,
    pub a_tokens: usize,
    pub b_tokens: usize,
    pub shared_payloads: usize,
    pub only_in_a: usize,
    pub only_in_b: usize,
    /// shared / union, in parts per million to keep the report integer-only.
    pub jaccard_ppm: u64,
    /// Shared payloads that also have the same id in both.
    pub shared_with_same_id: usize,
    pub examples_only_in_a: Vec<String>,
    pub examples_only_in_b: Vec<String>,
}

fn display(p: &TokenPayload) -> String {
    match p {
        TokenPayload::Bytes(b) => match std::str::from_utf8(b) {
            Ok(s) => format!("{s:?}"),
            Err(_) => format!("base64:{}", b64::encode(b)),
        },
        TokenPayload::Symbols(s) => format!("[{}]", s.join(", ")),
    }
}

pub fn compare(a: &Vocabulary, b: &Vocabulary) -> Result<Comparison> {
    let content = |v: &Vocabulary| -> BTreeMap<TokenPayload, u32> {
        let mut m = BTreeMap::new();
        for t in v.tokens.iter().filter(|t| t.kind != TokenKind::Special) {
            m.entry(t.payload.clone()).or_insert(t.id);
        }
        m
    };
    let (ma, mb) = (content(a), content(b));
    let shared: Vec<&TokenPayload> = ma.keys().filter(|k| mb.contains_key(*k)).collect();
    let only_a: Vec<&TokenPayload> = ma.keys().filter(|k| !mb.contains_key(*k)).collect();
    let only_b: Vec<&TokenPayload> = mb.keys().filter(|k| !ma.contains_key(*k)).collect();
    let union = ma.len() + mb.len() - shared.len();
    let longest = |v: &[&TokenPayload]| {
        let mut v: Vec<&TokenPayload> = v.to_vec();
        v.sort_by(|x, y| y.len().cmp(&x.len()).then(x.cmp(y)));
        v.into_iter().take(8).map(display).collect::<Vec<_>>()
    };
    Ok(Comparison {
        a: format!("{} {}", a.id(), a.version()),
        b: format!("{} {}", b.id(), b.version()),
        identical_content: a.compute_digest()? == b.compute_digest()?,
        same_algorithm: a.manifest.algorithm == b.manifest.algorithm,
        same_payload_kind: a.manifest.payload_kind == b.manifest.payload_kind,
        a_tokens: a.tokens.len(),
        b_tokens: b.tokens.len(),
        shared_payloads: shared.len(),
        only_in_a: only_a.len(),
        only_in_b: only_b.len(),
        jaccard_ppm: if union == 0 { 0 } else { (shared.len() as u64 * 1_000_000) / union as u64 },
        shared_with_same_id: shared.iter().filter(|k| ma[**k] == mb[**k]).count(),
        examples_only_in_a: longest(&only_a),
        examples_only_in_b: longest(&only_b),
    })
}

/// A vocabulary in the form tiktoken loads.
#[derive(Clone, Debug)]
pub struct TiktokenExport {
    /// Contents of a `.tiktoken` file: `base64(token bytes) rank` per line.
    pub bpe_file: Vec<u8>,
    /// The split pattern to pass as `pat_str`.
    pub pat_str: String,
    /// Reserved tokens and their ids, to pass as `special_tokens`.
    pub special_tokens: BTreeMap<String, u32>,
    pub n_vocab: u32,
}

/// Pattern equivalent to "no pre-tokenizer": the whole input is one piece.
pub const WHOLE_INPUT_PATTERN: &str = r"[\s\S]+";

/// Export a byte-BPE vocabulary for tiktoken.
///
/// Only vocabularies whose semantics tiktoken can reproduce are exported
/// (FR-007: "where semantics permit faithful conversion"): byte BPE, with no
/// two tokens sharing the same bytes, since tiktoken identifies a token by its
/// bytes alone.
pub fn export_tiktoken(v: &Vocabulary) -> Result<TiktokenExport> {
    v.validate()?;
    if !matches!(v.manifest.algorithm.as_str(), "bpe" | "rustbpe") || v.manifest.payload_kind != PayloadKind::Bytes {
        return Err(KuteError::Unsupported(format!(
            "tiktoken export needs a byte-BPE vocabulary; this one is {:?}",
            v.manifest.algorithm
        )));
    }
    let mut seen: BTreeSet<&[u8]> = BTreeSet::new();
    let mut file = Vec::new();
    let mut special_tokens = BTreeMap::new();
    for t in &v.tokens {
        let TokenPayload::Bytes(bytes) = &t.payload else {
            return Err(KuteError::InvalidVocabulary("symbol token in a byte vocabulary".into()));
        };
        if t.kind == TokenKind::Special {
            let name = String::from_utf8(bytes.clone())
                .map_err(|_| KuteError::Unsupported("tiktoken special tokens must be valid UTF-8".into()))?;
            special_tokens.insert(name, t.id);
            continue;
        }
        if !special_tokens.is_empty() {
            return Err(KuteError::Unsupported("tiktoken export needs reserved tokens to come after all other tokens".into()));
        }
        if !seen.insert(bytes.as_slice()) {
            return Err(KuteError::Unsupported(format!(
                "token {} has the same bytes as an earlier token; tiktoken cannot tell them apart, so the export would not be faithful",
                t.id
            )));
        }
        file.extend_from_slice(b64::encode(bytes).as_bytes());
        file.extend_from_slice(format!(" {}\n", t.id).as_bytes());
    }
    Ok(TiktokenExport {
        bpe_file: file,
        pat_str: v.pretokenizer.clone().unwrap_or_else(|| WHOLE_INPUT_PATTERN.to_string()),
        special_tokens,
        n_vocab: v.tokens.len() as u32,
    })
}
