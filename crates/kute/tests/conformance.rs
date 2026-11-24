//! The files in `specifications/kvf/vectors` are fixed. Every build must
//! read them, agree with their recorded digests, and decode the sequences to
//! the recorded bytes. The `v0.2.0` directory was written by the released
//! v0.2.0 binary, so this is also the backward-compatibility check.
//! `conformance/kvf_digest.py` checks the same digests without any KUTE code.

use kute::{Engine, TokenSequence};
use std::path::{Path, PathBuf};

fn vectors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specifications/kvf/vectors")
}

fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == extension))
        .collect();
    found.sort();
    found
}

#[test]
fn vocabularies_load_verify_and_keep_their_digests() {
    let engine = Engine::new();
    let mut seen = 0;
    for dir in [vectors(), vectors().join("v0.2.0")] {
        for path in files(&dir, "kvf") {
            let vocabulary = kute::vocab::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let recorded = vocabulary.integrity.as_ref().expect("vectors carry a digest").digest.clone();
            assert_eq!(vocabulary.compute_digest().unwrap(), recorded, "{}", path.display());
            let report = engine.verify(&vocabulary).unwrap();
            assert!(report.ok(), "{}: {:?}", path.display(), report.failures);
            // Writing the vocabulary again must not change its identity.
            let rewritten = kute::vocab::from_slice(&kute::vocab::to_vec(&vocabulary).unwrap()).unwrap();
            assert_eq!(rewritten.digest().unwrap(), recorded);
            seen += 1;
        }
    }
    assert_eq!(seen, 6);
}

#[test]
fn sequences_decode_to_the_recorded_bytes() {
    let engine = Engine::new();
    let mut seen = 0;
    for dir in [vectors(), vectors().join("v0.2.0")] {
        let vocabularies: Vec<_> = files(&dir, "kvf").iter().map(|p| kute::vocab::load(p).unwrap()).collect();
        for path in files(&dir, "kute") {
            let sequence = TokenSequence::from_bytes(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let vocabulary = vocabularies
                .iter()
                .find(|v| v.digest().unwrap() == sequence.vocabulary_digest)
                .unwrap_or_else(|| panic!("{}: no vector vocabulary has its digest", path.display()));
            let tokenizer = engine.tokenizer(vocabulary.clone()).unwrap();
            let decoded = engine.decode_raw(tokenizer.as_ref(), &sequence).unwrap();
            // `text-z.kute` and `text.kute` share `text.expected`.
            let stem = path.file_stem().unwrap().to_string_lossy().trim_end_matches("-z").to_string();
            let expected = std::fs::read(dir.join(format!("{stem}.expected"))).unwrap();
            assert_eq!(decoded, expected, "{}", path.display());
            seen += 1;
        }
    }
    assert_eq!(seen, 7);
}
