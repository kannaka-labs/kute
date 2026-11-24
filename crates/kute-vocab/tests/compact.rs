//! The compact binary vocabulary form: same model, same digest, bounded
//! reading of hostile input.

use kute_types::{KuteError, Vocabulary};
use kute_vocab::compact;
use std::path::PathBuf;

fn vector_files() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../specifications/kvf/vectors");
    let mut files = Vec::new();
    for dir in [root.clone(), root.join("v0.2.0")] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "kvf") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(files.len() >= 6, "expected the recorded vocabularies, found {}", files.len());
    files
}

fn vocabularies() -> Vec<Vocabulary> {
    vector_files().iter().map(|p| kute_vocab::load(p).unwrap()).collect()
}

#[test]
fn every_recorded_vocabulary_survives_the_compact_form() {
    for vocabulary in vocabularies() {
        let json = kute_vocab::to_vec(&vocabulary).unwrap();
        for deflate in [false, true] {
            let bytes = compact::to_vec(&vocabulary, deflate).unwrap();
            assert!(bytes.len() < json.len(), "compact form is not smaller for {}", vocabulary.id());
            let back = kute_vocab::from_slice(&bytes).unwrap();
            assert_eq!(back, vocabulary);
            assert_eq!(back.digest().unwrap(), vocabulary.digest().unwrap());
            // And back to JSON, byte for byte.
            assert_eq!(kute_vocab::to_vec(&back).unwrap(), json);
        }
    }
}

#[test]
fn save_picks_the_form_from_the_file_name() {
    let dir = std::env::temp_dir().join(format!("kute-compact-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vocabulary = vocabularies().remove(0);
    let binary = dir.join("v.kvfb");
    let text = dir.join("v.kvf");
    kute_vocab::save(&vocabulary, &binary, false).unwrap();
    kute_vocab::save(&vocabulary, &text, false).unwrap();
    assert!(compact::is_compact(&std::fs::read(&binary).unwrap()));
    assert_eq!(std::fs::read(&text).unwrap()[0], b'{');
    assert_eq!(kute_vocab::load(&binary).unwrap(), kute_vocab::load(&text).unwrap());
    // Saving the same content again is allowed; the file is immutable otherwise.
    kute_vocab::save(&vocabulary, &binary, false).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn altered_content_fails_the_digest() {
    for vocabulary in vocabularies() {
        let bytes = compact::to_vec(&vocabulary, false).unwrap();
        // Flip one bit in the last byte: part of the last token.
        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(kute_vocab::from_slice(&changed).is_err());
        // Trailing bytes and truncation are both refused.
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(kute_vocab::from_slice(&longer).is_err());
        assert!(kute_vocab::from_slice(&bytes[..bytes.len() - 1]).is_err());
        let deflated = compact::to_vec(&vocabulary, true).unwrap();
        let mut longer = deflated.clone();
        longer.push(0);
        assert!(kute_vocab::from_slice(&longer).is_err());
    }
}

#[test]
fn mutated_files_never_panic() {
    let mut state = 0x9e3779b97f4a7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for vocabulary in vocabularies() {
        if vocabulary.len() > 5000 {
            continue;
        }
        let bytes = compact::to_vec(&vocabulary, false).unwrap();
        let mut accepted = 0;
        for _ in 0..400 {
            let mut changed = bytes.clone();
            for _ in 0..=(next() % 3) {
                let at = (next() % changed.len() as u64) as usize;
                changed[at] = next() as u8;
            }
            if next() % 4 == 0 {
                changed.truncate((next() % changed.len() as u64) as usize);
            }
            // Must return, never panic or hang. Anything accepted has passed
            // the digest check, so it is the original content.
            if let Ok(v) = compact::from_slice(&changed, 16 * 1024 * 1024) {
                assert_eq!(v.digest().unwrap(), vocabulary.digest().unwrap());
                accepted += 1;
            }
        }
        assert!(accepted < 400);
    }
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// A hand-built file: one byte token, then `doublings` merge tokens, each
/// joining the previous token with itself.
fn doubling_file(doublings: usize) -> Vec<u8> {
    let header = br#"{"manifest":{"kvf_version":"0.1","id":"kute:test:bomb","version":"1.0.0","algorithm":"bpe","modality":"binary","payload_kind":"bytes","lossless":true,"generator":"test"}}"#;
    let mut body = Vec::new();
    varint(&mut body, header.len() as u64);
    body.extend_from_slice(header);
    varint(&mut body, 1 + doublings as u64);
    body.extend_from_slice(&[0, 0, 1, b'a']);
    for i in 0..doublings {
        body.push(4);
        varint(&mut body, i as u64);
        varint(&mut body, i as u64);
    }
    let mut file = b"KVFB\x01\x00".to_vec();
    file.extend_from_slice(&body);
    file
}

#[test]
fn a_small_file_cannot_expand_without_bound() {
    // Ten doublings is a legitimate 1 KiB token: this checks the hand-built
    // file is well formed, so the refusals below are for the right reason.
    let small = compact::from_slice(&doubling_file(10), 1 << 20).unwrap();
    assert_eq!(small.tokens[10].payload.len(), 1024);
    // Seventeen doublings would be a 128 KiB token: over the per-token limit.
    assert!(matches!(compact::from_slice(&doubling_file(17), 1 << 30), Err(KuteError::InvalidVocabulary(_))));
    // Within the per-token limit but over the caller's total budget.
    assert!(matches!(compact::from_slice(&doubling_file(14), 4096), Err(KuteError::LimitExceeded(_))));
}

#[test]
fn rules_out_of_id_order_are_refused_rather_than_reordered() {
    let mut vocabulary = vocabularies().into_iter().find(|v| v.merges.len() >= 2).unwrap();
    vocabulary.merges.swap(0, 1);
    vocabulary.integrity = None;
    assert!(matches!(compact::to_vec(&vocabulary, true), Err(KuteError::Unsupported(_))));
    // The JSON form keeps whatever order the rules have.
    kute_vocab::to_vec(&vocabulary).unwrap();
}
