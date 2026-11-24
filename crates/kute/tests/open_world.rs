//! Development directive v0.2, D03 and D05: a frozen vocabulary meets
//! symbols it has never seen, sequences from untrusted sources are bounded
//! before decoding, and private corpora stay out of published vocabularies.

use kute::{AdapterConfig, Engine, KuteError, Payload, TokenSequence, TrainingConfig, UnknownPolicy, VocabularyId};

fn events(names: &[&str]) -> Vec<u8> {
    names.iter().map(|n| format!("{{\"tag\":\"{n}\"}}\n")).collect::<String>().into_bytes()
}

fn trained(engine: &Engine, embed: bool) -> kute::Vocabulary {
    let mut log = Vec::new();
    for _ in 0..30 {
        log.extend(events(&["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"]));
    }
    let mut config = TrainingConfig::new(VocabularyId::parse("test:open-world:events").unwrap(), 16);
    config.embed_test_vectors = embed;
    engine.train("sequence-bpe", "jsonl-events", &AdapterConfig::default().with("field", "tag"), &[log], &config).unwrap()
}

/// Gate: "Frozen vocabulary encodes previously unseen symbols reversibly
/// with escape mode; strict mode rejects them."
#[test]
fn escape_round_trips_unseen_symbols_and_strict_rejects_them() {
    let engine = Engine::new();
    let vocabulary = trained(&engine, true);
    let digest = vocabulary.digest().unwrap();
    let table = vocabulary.len() as u32;
    let tokenizer = engine.tokenizer(vocabulary).unwrap();
    let names = ["MOTOR_START", "CURRENT_HIGH", "VALVE_7_OPEN", "MOTOR_TRIP", "OPERATOR_ACK", "VALVE_7_OPEN", "naïve ✓", "MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"];
    let raw = events(&names);

    let strict = engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Strict);
    assert!(matches!(strict, Err(KuteError::UnknownSymbol(s)) if s == "VALVE_7_OPEN"));
    assert!(matches!(engine.encode_raw(tokenizer.as_ref(), &raw), Err(KuteError::UnknownSymbol(_))));

    let open = engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Escape).unwrap();
    // Each distinct unknown symbol is stored once; the vocabulary is untouched.
    assert_eq!(open.escapes, ["VALVE_7_OPEN", "naïve ✓"]);
    assert_eq!(open.vocabulary_digest, digest);
    assert_eq!(tokenizer.vocabulary().unwrap().len() as u32, table);
    assert_eq!(open.ids.iter().filter(|&&id| id >= table).count(), 3);
    // Known runs on both sides of an escape are still merged.
    assert!(open.ids.len() < names.len());

    let Payload::Symbols(decoded) = tokenizer.decode(&open).unwrap().payload else { panic!("expected symbols") };
    assert_eq!(decoded, names);
    // The escape table survives both container formats.
    for bytes in [open.to_bytes().unwrap(), open.to_bytes_compressed().unwrap()] {
        let back = TokenSequence::from_bytes(&bytes).unwrap();
        assert_eq!(back, open);
        let Payload::Symbols(again) = tokenizer.decode(&back).unwrap().payload else { panic!("expected symbols") };
        assert_eq!(again, names);
    }

    // With nothing unknown, escape mode gives exactly the strict encoding.
    let known = events(&["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"]);
    let a = engine.encode_raw(tokenizer.as_ref(), &known).unwrap();
    let b = engine.encode_raw_open(tokenizer.as_ref(), &known, UnknownPolicy::Escape).unwrap();
    assert_eq!(a, b);

    // An id past the escape table is an error, not a guess.
    let mut forged = open.clone();
    forged.ids.push(table + 2);
    assert!(matches!(tokenizer.decode(&forged), Err(KuteError::UnknownToken(_))));
}

/// Byte-level algorithms never meet an unknown symbol, so both policies
/// agree. Table algorithms without escape support say so.
#[test]
fn policies_on_other_algorithms() {
    let engine = Engine::new();
    let config = TrainingConfig::new(VocabularyId::parse("test:open-world:text").unwrap(), 300);
    let text = b"the quick brown fox jumps over the lazy dog. ".repeat(40);
    let bpe = engine.train("bpe", "text", &AdapterConfig::default(), &[text], &config).unwrap();
    let tokenizer = engine.tokenizer(bpe).unwrap();
    let unseen = "zażółć gęślą jaźń \u{1F980}".as_bytes();
    let strict = engine.encode_raw(tokenizer.as_ref(), unseen).unwrap();
    let open = engine.encode_raw_open(tokenizer.as_ref(), unseen, UnknownPolicy::Escape).unwrap();
    assert_eq!(strict, open);
    assert!(open.escapes.is_empty());
    assert_eq!(engine.decode_raw(tokenizer.as_ref(), &open).unwrap(), unseen);

    let mut config = TrainingConfig::new(VocabularyId::parse("test:open-world:dictionary").unwrap(), 8);
    config.dictionary = vec!["OPEN".into(), "CLOSE".into()];
    let adapter = AdapterConfig::default().with("field", "tag");
    let dictionary = engine.train("dictionary", "jsonl-events", &adapter, &[events(&["OPEN", "CLOSE"])], &config).unwrap();
    let tokenizer = engine.tokenizer(dictionary).unwrap();
    let raw = events(&["OPEN", "JAMMED"]);
    assert!(matches!(engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Strict), Err(KuteError::UnknownSymbol(_))));
    assert!(matches!(engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Escape), Err(KuteError::Unsupported(_))));
}

/// A short sequence can stand for a very large output. The budget check
/// runs on the token table, before any output is built.
#[test]
fn decode_budget_is_checked_before_decoding() {
    let engine = Engine::new();
    let config = TrainingConfig::new(VocabularyId::parse("test:open-world:budget").unwrap(), 400);
    let text = b"abcdefgh".repeat(4000);
    let vocabulary = engine.train("bpe", "binary", &AdapterConfig::default(), &[text], &config).unwrap();
    let longest = vocabulary.tokens.iter().max_by_key(|t| t.payload.len()).unwrap();
    assert!(longest.payload.len() >= 64);
    let tokenizer = engine.tokenizer(vocabulary.clone()).unwrap();
    let mut bomb = engine.encode_raw(tokenizer.as_ref(), b"a").unwrap();
    bomb.ids = vec![longest.id; 10_000];
    let units = kute::decoded_units(&vocabulary, &bomb).unwrap();
    assert_eq!(units, 10_000 * longest.payload.len() as u64);
    assert!(matches!(engine.decode_raw_bounded(tokenizer.as_ref(), &bomb, units - 1), Err(KuteError::LimitExceeded(_))));
    assert_eq!(engine.decode_raw_bounded(tokenizer.as_ref(), &bomb, units).unwrap().len() as u64, units);
}

/// Embedded test vectors are verbatim excerpts of the corpus. Turning them
/// off must leave no corpus text in the published file.
#[test]
fn private_corpora_are_not_embedded_when_asked() {
    let engine = Engine::new();
    let with = String::from_utf8(kute::vocab::to_vec(&trained(&engine, true)).unwrap()).unwrap();
    let without_vocab = trained(&engine, false);
    assert!(without_vocab.test_vectors.is_empty());
    assert!(engine.verify(&without_vocab).unwrap().ok());
    let without = String::from_utf8(kute::vocab::to_vec(&without_vocab).unwrap()).unwrap();
    assert!(with.contains("\"ids\""));
    assert!(!without.contains("\"ids\""));

    let secret = b"account 4417 belongs to the confidential ledger. ".repeat(50);
    let mut config = TrainingConfig::new(VocabularyId::parse("test:open-world:private").unwrap(), 280);
    config.embed_test_vectors = false;
    let vocabulary = engine.train("bpe", "text", &AdapterConfig::default(), &[secret], &config).unwrap();
    assert!(vocabulary.test_vectors.is_empty());
    // Learned tokens still reveal frequent fragments; that is inherent to a
    // vocabulary and is documented. No whole sentence is stored.
    let longest = vocabulary.tokens.iter().map(|t| t.payload.len()).max().unwrap();
    assert!(longest < 49, "a token holds the whole secret sentence");
}
