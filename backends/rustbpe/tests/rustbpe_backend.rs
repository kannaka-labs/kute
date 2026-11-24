//! MVP criterion: rustbpe integrated behind the generic tokenizer interface.
//!
//! Needs a Python installation.

use kute_bpe::BpeTokenizer;
use kute_rustbpe::RustBpeTokenizer;
use kute_core::{Tokenizer, VecCorpus};
use kute_types::{CanonicalInput, KuteError, Payload, Pretokenizer, TrainingConfig, VocabularyId};

const TEXT: &str = "The engine reads the event log. The engine writes the event log. \
The motor started, the current rose, the motor tripped. The motor started, the current rose, the motor tripped. \
Naïve café — 日本語のテキスト, emoji 🙂. fn main() { println!(\"hello, world\"); }\n";

fn corpus() -> VecCorpus {
    VecCorpus::new(vec![CanonicalInput::text(TEXT), CanonicalInput::text(TEXT), CanonicalInput::text("a second, shorter document about the engine")])
}

fn config(id: &str) -> TrainingConfig {
    let mut c = TrainingConfig::new(VocabularyId::parse(id).unwrap(), 400);
    c.pretokenizer = Pretokenizer::Gpt4;
    c
}

/// Both backends are driven through `dyn Tokenizer` only.
fn train(mut tokenizer: Box<dyn Tokenizer>, id: &str) -> Box<dyn Tokenizer> {
    tokenizer.train(&mut corpus(), &config(id)).unwrap();
    tokenizer
}

#[test]
fn rustbpe_trains_encodes_and_decodes_through_the_generic_interface() {
    let tokenizer = train(RustBpeTokenizer::boxed(), "test:rustbpe");
    let vocabulary = tokenizer.vocabulary().unwrap();
    assert_eq!(vocabulary.manifest.algorithm, "rustbpe");
    // The corpus is small: training stops when it runs out of pairs.
    assert!(vocabulary.tokens.len() > 350 && vocabulary.tokens.len() <= 400, "{}", vocabulary.tokens.len());
    assert!(vocabulary.validate().is_ok());
    for case in [TEXT, "", "unseen words: zyzzyva", "混合 text 🙂"] {
        let tokens = tokenizer.encode(&CanonicalInput::text(case)).unwrap();
        assert_eq!(tokenizer.decode(&tokens).unwrap().payload, Payload::Bytes(case.as_bytes().to_vec()));
    }
    let tokens = tokenizer.encode(&CanonicalInput::text(TEXT)).unwrap();
    assert!(tokens.ids.len() * 2 < TEXT.len());
    // Outside rustbpe's scope: refused, not mangled.
    let binary = CanonicalInput::bytes(kute_types::Modality::Binary, vec![0xff, 0xfe]);
    assert!(matches!(tokenizer.encode(&binary), Err(KuteError::InvalidInput(_))));
}

#[test]
fn native_backend_learns_the_same_merges_as_rustbpe() {
    let reference = train(RustBpeTokenizer::boxed(), "test:parity");
    let native = train(BpeTokenizer::boxed(), "test:parity");
    let (r, n) = (reference.vocabulary().unwrap(), native.vocabulary().unwrap());
    assert_eq!(r.merges, n.merges, "merge tables differ");
    let payloads = |v: &kute_types::Vocabulary| v.tokens.iter().map(|t| t.payload.clone()).collect::<Vec<_>>();
    assert_eq!(payloads(r), payloads(n));
    for case in [TEXT, "the engine tripped; the log wrote itself", "日本語 and café"] {
        let input = CanonicalInput::text(case);
        assert_eq!(reference.encode(&input).unwrap().ids, native.encode(&input).unwrap().ids, "{case}");
    }
}

#[test]
fn vocabularies_are_interchangeable_between_backends() {
    let reference = train(RustBpeTokenizer::boxed(), "test:swap");
    let mut native = BpeTokenizer::new();
    native.load(reference.vocabulary().unwrap().clone()).unwrap();
    let mut back = RustBpeTokenizer::new();
    back.load(native.vocabulary().unwrap().clone()).unwrap();
    let input = CanonicalInput::text(TEXT);
    let a = reference.encode(&input).unwrap();
    assert_eq!(a, native.encode(&input).unwrap());
    assert_eq!(a, back.encode(&input).unwrap());
    assert_eq!(native.decode(&a).unwrap(), input);
}
