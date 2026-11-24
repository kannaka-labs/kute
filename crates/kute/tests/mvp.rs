//! Tests for the v0.1 MVP acceptance criteria (PRD section 16). Each test
//! names the criterion it covers.

use kute::{
    vocab, AdapterConfig, CanonicalInput, CorpusSource, Engine, KuteError, Modality, Payload, PayloadKind,
    Pretokenizer, TokenKind, TokenPayload, TokenSequence, Tokenizer, TokenizerCapabilities, TrainingConfig,
    Version, Vocabulary, VocabularyId,
};

const TEXT: &str = "The engine reads the event log. The engine writes the event log. \
A receipt that names its own gaps is worth more than one that hides them. \
The motor started, the current rose, the motor tripped. The motor started, the current rose, the motor tripped. \
Naïve café — 日本語のテキスト, emoji 🙂, tabs\tand\r\nWindows line ends. \
fn main() { println!(\"hello, world\"); } fn main() { println!(\"hello, kute\"); }\n";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

fn config(id: &str, size: u32) -> TrainingConfig {
    // These tests exercise the embedded test vectors, which are opt-in.
    let mut config = TrainingConfig::new(VocabularyId::parse(id).unwrap(), size);
    config.embed_test_vectors = true;
    config
}

fn text_vocab(engine: &Engine) -> Vocabulary {
    let mut cfg = config("test:text", 400);
    cfg.pretokenizer = Pretokenizer::Gpt4;
    engine.train("bpe", "text", &AdapterConfig::default(), &[TEXT.repeat(3).into_bytes()], &cfg).unwrap()
}

fn round_trip(engine: &Engine, tokenizer: &dyn Tokenizer, raw: &[u8]) -> TokenSequence {
    let tokens = engine.encode_raw(tokenizer, raw).unwrap();
    assert_eq!(engine.decode_raw(tokenizer, &tokens).unwrap(), raw, "round trip changed the data");
    // The serialized container must survive too.
    let restored = TokenSequence::from_bytes(&tokens.to_bytes().unwrap()).unwrap();
    assert_eq!(restored, tokens);
    tokens
}

/// Criterion: text and binary round-trip tests pass without loss.
#[test]
fn text_round_trips_without_loss() {
    let engine = Engine::new();
    let tokenizer = engine.tokenizer(text_vocab(&engine)).unwrap();
    let cases = [
        "",
        " ",
        "a",
        TEXT,
        "unseen words: zyzzyva quokka\u{0}\u{7f}",
        "   leading and trailing   ",
        "\n\n\r\n\t",
        "混合 text with ελληνικά and العربية and 🙂🙃",
        "e\u{301} combining, and a lone surrogate-free astral 𝒳",
    ];
    for case in cases {
        let tokens = round_trip(&engine, tokenizer.as_ref(), case.as_bytes());
        assert!(tokens.ids.len() <= case.len());
    }
    // Compression on in-distribution text: fewer tokens than bytes.
    let tokens = engine.encode_raw(tokenizer.as_ref(), TEXT.as_bytes()).unwrap();
    assert!(tokens.ids.len() * 2 < TEXT.len(), "{} tokens for {} bytes", tokens.ids.len(), TEXT.len());
}

/// Criterion: text and binary round-trip tests pass without loss.
#[test]
fn binary_round_trips_without_loss() {
    let engine = Engine::new();
    let mut rng = Rng(0x5eed);
    let mut training = Vec::new();
    for _ in 0..40 {
        training.extend_from_slice(b"\x7fELF\x02\x01\x01\x00");
        training.extend_from_slice(&rng.bytes(24));
        training.extend_from_slice(&[0u8; 16]);
    }
    let vocab = engine.train("bpe", "binary", &AdapterConfig::default(), &[training.clone()], &config("test:binary", 320)).unwrap();
    assert_eq!(vocab.pretokenizer, None);
    let tokenizer = engine.tokenizer(vocab).unwrap();
    let all_bytes: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
    let invalid_utf8 = vec![0xff, 0xfe, 0xc0, 0x80, 0xed, 0xa0, 0x80, 0xf8];
    for case in [vec![], vec![0], all_bytes, invalid_utf8, training.clone(), rng.bytes(70_000), vec![0u8; 100_000]] {
        round_trip(&engine, tokenizer.as_ref(), &case);
    }
    let tokens = engine.encode_raw(tokenizer.as_ref(), &training).unwrap();
    assert!(tokens.ids.len() < training.len());
}

/// A text vocabulary must also survive input that is not text.
#[test]
fn text_vocabulary_still_round_trips_arbitrary_bytes() {
    let engine = Engine::new();
    let mut cfg = config("test:text-binary", 300);
    cfg.pretokenizer = Pretokenizer::Gpt4;
    let vocab = engine.train("bpe", "binary", &AdapterConfig::default(), &[TEXT.as_bytes().to_vec()], &cfg).unwrap();
    let tokenizer = engine.tokenizer(vocab).unwrap();
    let mut rng = Rng(7);
    let mut mixed = TEXT.as_bytes().to_vec();
    mixed.extend_from_slice(&rng.bytes(500));
    round_trip(&engine, tokenizer.as_ref(), &mixed);
}

/// A pattern that matches almost nothing must not lose the unmatched text.
#[test]
fn pretokenizer_gaps_are_kept() {
    let engine = Engine::new();
    let mut cfg = config("test:gaps", 280);
    cfg.pretokenizer = Pretokenizer::Regex("[0-9]+".into());
    let vocab = engine.train("bpe", "text", &AdapterConfig::default(), &[b"abc 123 def 123 ghi".to_vec()], &cfg).unwrap();
    let tokenizer = engine.tokenizer(vocab).unwrap();
    round_trip(&engine, tokenizer.as_ref(), b"letters 42 only-letters, then 7 and the end");
    let mut bad = config("test:bad-regex", 280);
    bad.pretokenizer = Pretokenizer::Regex("(unclosed".into());
    assert!(matches!(
        engine.train("bpe", "text", &AdapterConfig::default(), &[b"x".to_vec()], &bad),
        Err(KuteError::InvalidConfig(_))
    ));
}

/// Criterion: structured JSON records encoded using an adapter.
#[test]
fn structured_json_through_adapter() {
    let engine = Engine::new();
    let records = "{\"agent\": \"kannaka\", \"action\": \"observe\", \"target\": \"sensor-1\"}\n\
{\"target\": \"sensor-2\", \"agent\": \"kannaka\", \"action\": \"observe\"}\n\
{\"agent\":\"flaukowski\",\"action\":\"report\",\"target\":\"sensor-1\"}\n";
    let vocab = engine.train("bpe", "jsonl", &AdapterConfig::default(), &[records.repeat(8).into_bytes()], &config("test:agent-messages", 330)).unwrap();
    let spec = vocab.adapter.clone().unwrap();
    assert_eq!(spec.name, "jsonl");
    assert!(!spec.lossless, "canonicalizing JSON must be declared lossy at the byte level");
    assert_eq!(vocab.manifest.modality, Modality::Structured);
    let tokenizer = engine.tokenizer(vocab).unwrap();
    let tokens = engine.encode_raw(tokenizer.as_ref(), records.as_bytes()).unwrap();
    let decoded = engine.decode_raw(tokenizer.as_ref(), &tokens).unwrap();
    // Byte-level formatting is normalized; every value is kept.
    let parse = |b: &[u8]| -> Vec<serde_json::Value> {
        std::str::from_utf8(b).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    assert_eq!(parse(&decoded), parse(records.as_bytes()));
    assert!(tokens.ids.len() * 3 < decoded.len(), "{} tokens for {} bytes", tokens.ids.len(), decoded.len());
    assert!(engine.encode_raw(tokenizer.as_ref(), b"{broken").is_err());
}

/// Event sequences: the FR-005 example, a composite token made of events.
#[test]
fn event_sequences_become_composite_tokens() {
    let engine = Engine::new();
    let mut log = String::new();
    for i in 0..30 {
        for event in ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"] {
            log.push_str(&format!("{{\"tag\":\"{event}\",\"seq\":{i}}}\n"));
        }
        log.push_str("{\"tag\":\"OPERATOR_ACK\"}\n");
    }
    let adapter = AdapterConfig::default().with("field", "tag");
    let vocab = engine.train("sequence-bpe", "jsonl-events", &adapter, &[log.clone().into_bytes()], &config("kute:industrial:events", 16)).unwrap();
    assert_eq!(vocab.manifest.payload_kind, PayloadKind::Symbols);
    let episode = vec!["MOTOR_START".to_string(), "CURRENT_HIGH".to_string(), "MOTOR_TRIP".to_string()];
    let composite = vocab.tokens.iter().find(|t| t.payload == TokenPayload::Symbols(episode.clone())).expect("a token for the whole failure episode");
    assert_eq!(composite.kind, TokenKind::Merge);
    let record = vocab.record(composite.id).unwrap();
    assert_eq!(record.vocabulary_id.as_str(), "kute:industrial:events");
    assert_eq!(record.payload, serde_json::json!(episode));
    assert!(record.content_hash.starts_with("sha256:"));

    let tokenizer = engine.tokenizer(vocab).unwrap();
    let tokens = engine.encode_raw(tokenizer.as_ref(), log.as_bytes()).unwrap();
    assert!(tokens.ids.len() < 40, "120 events should compress to a handful of tokens, got {}", tokens.ids.len());
    let decoded = tokenizer.decode(&tokens).unwrap();
    let Payload::Symbols(symbols) = decoded.payload else { panic!("expected symbols") };
    assert_eq!(symbols.len(), 120);
    assert_eq!(&symbols[..4], ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"]);
    // A symbol outside the alphabet is refused, never approximated.
    let unknown = engine.encode_raw(tokenizer.as_ref(), b"{\"tag\":\"NEVER_SEEN\"}\n");
    assert!(matches!(unknown, Err(KuteError::UnknownSymbol(_))));
}

/// Criterion: train, save, load and inspect custom vocabularies.
#[test]
fn train_save_load_inspect() {
    let engine = Engine::new();
    let vocab = text_vocab(&engine);
    let dir = tempdir("save-load");
    let path = dir.join("text.kvf");
    vocab::save(&vocab, &path, false).unwrap();
    let loaded = vocab::load(&path).unwrap();
    assert_eq!(loaded, vocab);
    assert_eq!(loaded.digest().unwrap(), vocab.digest().unwrap());

    let summary = vocab::summarize(&loaded).unwrap();
    assert_eq!(summary.id, "test:text");
    assert_eq!(summary.algorithm, "bpe");
    assert_eq!(summary.tokens, loaded.tokens.len());
    assert_eq!(summary.tokens_by_kind["byte"], 256);
    assert!(summary.merges > 50);
    assert!(summary.test_vectors >= 1);
    assert_eq!(summary.corpus_fingerprints.len(), 1);

    // A loaded vocabulary encodes exactly like the one that was trained.
    let a = engine.tokenizer(vocab.clone()).unwrap();
    let b = engine.tokenizer(loaded).unwrap();
    assert_eq!(engine.encode_raw(a.as_ref(), TEXT.as_bytes()).unwrap(), engine.encode_raw(b.as_ref(), TEXT.as_bytes()).unwrap());

    // Saving the same content again is fine; different content under the same
    // path is refused because versions are immutable.
    vocab::save(&vocab, &path, false).unwrap();
    let mut other = vocab.clone();
    other.manifest.description = Some("changed".into());
    assert!(vocab::save(&other, &path, false).is_err());
    vocab::save(&other, &path, true).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// Criterion: reproducible training (determinism under supported configurations).
#[test]
fn training_is_deterministic() {
    let engine = Engine::new();
    let a = vocab::to_vec(&text_vocab(&engine)).unwrap();
    let b = vocab::to_vec(&text_vocab(&Engine::new())).unwrap();
    assert_eq!(a, b, "two training runs produced different files");
    // Same tokens whether the corpus arrives as one document or many.
    let mut cfg = config("test:text", 400);
    cfg.pretokenizer = Pretokenizer::Gpt4;
    let one = engine.train("bpe", "text", &AdapterConfig::default(), &[TEXT.repeat(3).into_bytes()], &cfg).unwrap();
    let many = engine.train("bpe", "text", &AdapterConfig::default(), &vec![TEXT.as_bytes().to_vec(); 3], &cfg).unwrap();
    assert_eq!(one.tokens, many.tokens);
    assert_eq!(one.merges, many.merges);
    assert_ne!(one.digest().unwrap(), many.digest().unwrap(), "different corpus framing must show in the fingerprint");
}

/// Criterion: malformed inputs and vocabulary files handled safely.
#[test]
fn malformed_vocabulary_files_are_rejected_not_trusted() {
    let engine = Engine::new();
    let good = vocab::to_vec(&text_vocab(&engine)).unwrap();
    assert!(vocab::from_slice(&good).is_ok());

    for junk in [&b""[..], b"{", b"null", b"[]", b"{\"manifest\":{}}", b"\xff\xfe\x00\x01", b"{\"tokens\":[]}"] {
        assert!(vocab::from_slice(junk).is_err());
    }
    // Truncation at every 997th byte and just before the closing brace (the
    // file ends "}\n", so losing only the final newline is still a whole document).
    for cut in (0..good.len()).step_by(997).chain([good.len() - 2, good.len() - 3]) {
        assert!(vocab::from_slice(&good[..cut]).is_err(), "truncated at {cut} was accepted");
    }
    // Random single-byte damage: every outcome must be an error or a document
    // that still passes validation; never a panic, never a silent change of
    // token content (the digest catches those).
    let original = vocab::from_slice(&good).unwrap();
    let mut rng = Rng(0xbad5eed);
    let mut rejected = 0;
    for _ in 0..600 {
        let mut damaged = good.clone();
        let at = (rng.next() as usize) % damaged.len();
        damaged[at] = (rng.next() >> 16) as u8;
        match vocab::from_slice(&damaged) {
            Err(_) => rejected += 1,
            Ok(v) => assert_eq!(v, original, "damage at byte {at} changed the vocabulary and was accepted"),
        }
    }
    assert!(rejected > 300, "only {rejected} of 600 damaged files were rejected");

    // Targeted structural attacks, with the digest recomputed so only the
    // structure check can catch them.
    let tamper = |f: &dyn Fn(&mut Vocabulary)| {
        let mut v = original.clone();
        f(&mut v);
        v.integrity = None;
        v.validate()
    };
    assert!(tamper(&|v| v.tokens[300].id = 9).is_err());
    assert!(tamper(&|v| v.merges[0].left = v.merges[0].id).is_err(), "self-referential merge");
    assert!(tamper(&|v| v.merges[0].right = 1_000_000).is_err());
    assert!(tamper(&|v| v.tokens[260].payload = TokenPayload::Bytes(b"forged".to_vec())).is_err());
    assert!(tamper(&|v| { let m = v.merges[0]; v.merges.push(m); }).is_err());
    assert!(tamper(&|v| v.merges.clear()).is_err());
    assert!(tamper(&|v| v.manifest.kvf_version = "9.9".into()).is_err());
    assert!(tamper(&|v| v.tokens.clear()).is_err());
    assert!(tamper(&|v| v.test_vectors[0].ids.push(u32::MAX)).is_err());
    assert!(tamper(&|v| v.tokens[5].payload = TokenPayload::Bytes(vec![])).is_err());
    // An honest edit without resealing is caught by the digest.
    let mut edited = original.clone();
    edited.manifest.description = Some("edited after signing".into());
    assert!(matches!(edited.validate(), Err(KuteError::Integrity(_))));
    // A byte table that is not the identity is refused by the tokenizer.
    let mut swapped = original.clone();
    swapped.tokens[65].payload = TokenPayload::Bytes(vec![66]);
    swapped.tokens[66].payload = TokenPayload::Bytes(vec![65]);
    swapped.integrity = None;
    for m in swapped.merges.clone() {
        // keep merge payloads consistent so only the byte table is wrong
        let (TokenPayload::Bytes(a), TokenPayload::Bytes(b)) = (swapped.tokens[m.left as usize].payload.clone(), swapped.tokens[m.right as usize].payload.clone()) else { panic!() };
        swapped.tokens[m.id as usize].payload = TokenPayload::Bytes([a, b].concat());
    }
    swapped.test_vectors.clear();
    assert!(swapped.validate().is_ok());
    assert!(engine.tokenizer(swapped).is_err());
}

/// Criterion: malformed inputs handled safely (token sequences and decode).
#[test]
fn malformed_token_sequences_are_rejected() {
    let engine = Engine::new();
    let vocab = text_vocab(&engine);
    let tokenizer = engine.tokenizer(vocab.clone()).unwrap();
    let good = engine.encode_raw(tokenizer.as_ref(), b"the engine").unwrap();

    let mut unknown = good.clone();
    unknown.ids.push(vocab.tokens.len() as u32);
    assert!(matches!(tokenizer.decode(&unknown), Err(KuteError::UnknownToken(_))));

    // Ids from another vocabulary are never accepted, even when the numbers fit.
    let other = engine.train("bytes", "binary", &AdapterConfig::default(), &[], &config("test:other", 256)).unwrap();
    let other_tok = engine.tokenizer(other).unwrap();
    assert!(matches!(other_tok.decode(&good), Err(KuteError::VocabularyMismatch(_))));
    let mut same_name = good.clone();
    same_name.vocabulary_digest = "sha256:0000".into();
    assert!(matches!(tokenizer.decode(&same_name), Err(KuteError::VocabularyMismatch(_))));
    let mut wrong_version = good.clone();
    wrong_version.vocabulary_version = Version::new(9, 0, 0);
    assert!(matches!(tokenizer.decode(&wrong_version), Err(KuteError::VocabularyMismatch(_))));

    // Wrong payload shape for the algorithm.
    assert!(tokenizer.encode(&CanonicalInput::symbols(["A", "B"])).is_err());
    // An untrained tokenizer reports that instead of panicking.
    let fresh = engine.registry().tokenizer("bpe").unwrap();
    assert!(matches!(fresh.encode(&CanonicalInput::text("x")), Err(KuteError::NotTrained(_))));
    assert!(fresh.vocabulary().is_none());
}

/// Criterion: vocabulary identifiers and versions validated.
#[test]
fn identifiers_and_versions_are_validated() {
    assert!(VocabularyId::parse("kute:industrial:events").is_ok());
    for bad in ["", "plain", "Kute:Upper", "kute:has space", "kute::double", "kute:../../etc"] {
        assert!(matches!(VocabularyId::parse(bad), Err(KuteError::InvalidIdentifier(_))), "{bad}");
    }
    // The same rules apply when a file is read.
    let engine = Engine::new();
    let text = String::from_utf8(vocab::to_vec(&text_vocab(&engine)).unwrap()).unwrap();
    assert!(vocab::from_slice(text.replace("\"test:text\"", "\"Not A Valid Id\"").as_bytes()).is_err());
    assert!(vocab::from_slice(text.replace("\"version\": \"0.1.0\"", "\"version\": \"one\"").as_bytes()).is_err());
    assert!(vocab::from_slice(text.replace("\"version\": \"0.1.0\"", "\"version\": \"01.1.0\"").as_bytes()).is_err());
    // Training configuration is checked before any work starts.
    let mut cfg = config("test:cfg", 100);
    assert!(engine.train("bpe", "text", &AdapterConfig::default(), &[b"x".to_vec()], &cfg).is_err(), "vocab_size below 256");
    cfg.vocab_size = 300;
    cfg.min_frequency = 0;
    assert!(engine.train("bpe", "text", &AdapterConfig::default(), &[b"x".to_vec()], &cfg).is_err());
    cfg.min_frequency = 1;
    cfg.reserved_tokens = vec!["<|a|>".into(), "<|a|>".into()];
    assert!(engine.train("bpe", "text", &AdapterConfig::default(), &[b"x".to_vec()], &cfg).is_err());
    cfg.reserved_tokens.clear();
    cfg.max_corpus_units = Some(3);
    assert!(matches!(
        engine.train("bpe", "text", &AdapterConfig::default(), &[b"more than three".to_vec()], &cfg),
        Err(KuteError::LimitExceeded(_))
    ));
    assert!(engine.train("no-such-algorithm", "text", &AdapterConfig::default(), &[], &config("test:x", 300)).is_err());
    assert!(engine.train("bpe", "no-such-adapter", &AdapterConfig::default(), &[], &config("test:x", 300)).is_err());
}

/// Criterion: compatible vocabulary export to tiktoken.
#[test]
fn tiktoken_export_is_well_formed() {
    use std::collections::BTreeSet;
    let engine = Engine::new();
    let mut cfg = config("test:tiktoken", 420);
    cfg.pretokenizer = Pretokenizer::Gpt4;
    cfg.reserved_tokens = vec!["<|endoftext|>".into(), "<|pad|>".into()];
    let vocab = engine.train("bpe", "text", &AdapterConfig::default(), &[TEXT.repeat(2).into_bytes()], &cfg).unwrap();
    let export = vocab::export_tiktoken(&vocab).unwrap();
    assert_eq!(export.pat_str, kute::config::GPT4_PATTERN);
    assert_eq!(export.n_vocab as usize, vocab.tokens.len());
    assert_eq!(export.special_tokens.len(), 2);
    assert_eq!(export.special_tokens["<|endoftext|>"] as usize, vocab.tokens.len() - 2);

    let text = String::from_utf8(export.bpe_file).unwrap();
    let mut seen = BTreeSet::new();
    let mut ranks = Vec::new();
    for line in text.lines() {
        let (token, rank) = line.split_once(' ').unwrap();
        assert!(seen.insert(token.to_string()), "duplicate token bytes in export");
        ranks.push(rank.parse::<u32>().unwrap());
    }
    assert_eq!(ranks, (0..ranks.len() as u32).collect::<Vec<_>>(), "ranks must be dense and ordered");
    assert_eq!(ranks.len(), vocab.tokens.len() - 2);

    // Vocabularies tiktoken cannot represent faithfully are refused.
    let events = engine.train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &[b"{\"event\":\"A\"}\n{\"event\":\"B\"}\n".to_vec()], &config("test:events", 8)).unwrap();
    assert!(matches!(vocab::export_tiktoken(&events), Err(KuteError::Unsupported(_))));
}

/// The baseline algorithms: bytes, char, dictionary.
#[test]
fn baseline_algorithms_round_trip() {
    let engine = Engine::new();
    let none = AdapterConfig::default();
    let mut rng = Rng(99);
    let noise = rng.bytes(4096);

    let bytes = engine.tokenizer(engine.train("bytes", "binary", &none, &[], &config("test:bytes", 256)).unwrap()).unwrap();
    assert_eq!(round_trip(&engine, bytes.as_ref(), &noise).ids.len(), noise.len());

    let chars = engine.tokenizer(engine.train("char", "binary", &none, &[TEXT.as_bytes().to_vec()], &config("test:char", 512)).unwrap()).unwrap();
    let jp = "日本語のテキスト";
    assert_eq!(round_trip(&engine, chars.as_ref(), jp.as_bytes()).ids.len(), jp.chars().count());
    round_trip(&engine, chars.as_ref(), "unseen: ελληνικά".as_bytes());
    round_trip(&engine, chars.as_ref(), &noise);

    let mut dict_cfg = config("test:dict", 300);
    dict_cfg.dictionary = vec!["MOTOR_START".into(), "MOTOR".into(), "CURRENT_HIGH".into(), " ".into()];
    let dict = engine.tokenizer(engine.train("dictionary", "binary", &none, &[], &dict_cfg).unwrap()).unwrap();
    let tokens = round_trip(&engine, dict.as_ref(), b"MOTOR_START MOTOR CURRENT_HIGH?");
    // longest match wins: MOTOR_START, " ", MOTOR, " ", CURRENT_HIGH, "?"
    assert_eq!(tokens.ids.len(), 6);
    round_trip(&engine, dict.as_ref(), &noise);

    let sym = engine.train("dictionary", "jsonl-events", &none, &[b"{\"event\":\"B\"}\n{\"event\":\"A\"}\n{\"event\":\"B\"}\n".to_vec()], &config("test:symdict", 8)).unwrap();
    assert_eq!(sym.tokens.len(), 2);
    let sym = engine.tokenizer(sym).unwrap();
    assert_eq!(engine.encode_raw(sym.as_ref(), b"{\"event\":\"A\"}\n{\"event\":\"B\"}\n").unwrap().ids, vec![0, 1]);
    assert!(matches!(engine.encode_raw(sym.as_ref(), b"{\"event\":\"C\"}\n"), Err(KuteError::UnknownSymbol(_))));
}

/// Training options: min_frequency, max_token_length, reserved tokens.
#[test]
fn training_options_are_honoured() {
    let engine = Engine::new();
    let corpus = [b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bcbcbcbc xy".to_vec()];
    let mut cfg = config("test:options", 300);
    cfg.max_token_length = Some(4);
    cfg.min_frequency = 2;
    cfg.reserved_tokens = vec!["<|eot|>".into()];
    let vocab = engine.train("bpe", "text", &AdapterConfig::default(), &corpus, &cfg).unwrap();
    assert!(vocab.tokens.iter().filter(|t| t.kind == TokenKind::Merge).all(|t| t.payload.len() <= 4));
    assert!(vocab.tokens.iter().filter(|t| t.kind == TokenKind::Merge).all(|t| t.frequency.unwrap() >= 2));
    assert!(!vocab.tokens.iter().any(|t| t.payload == TokenPayload::Bytes(b"xy".to_vec())), "a pair seen once must not become a token");
    let last = vocab.tokens.last().unwrap();
    assert_eq!((last.kind, &last.payload), (TokenKind::Special, &TokenPayload::Bytes(b"<|eot|>".to_vec())));
    // Reserved tokens are never produced from content that merely looks like them.
    let tokenizer = engine.tokenizer(vocab.clone()).unwrap();
    let tokens = round_trip(&engine, tokenizer.as_ref(), b"<|eot|>");
    assert!(!tokens.ids.contains(&last.id));
    assert!(vocab.tokens.len() < 300, "training stops when no pair qualifies");
}

/// FR-002: a third party registers an algorithm without modifying the core.
#[test]
fn third_party_algorithm_registers_without_core_changes() {
    #[derive(Default)]
    struct Reverse(Option<Box<dyn Tokenizer>>);

    impl Tokenizer for Reverse {
        fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> kute::Result<Vocabulary> {
            let mut inner = Engine::new().registry().tokenizer("bytes")?;
            let mut v = inner.train(corpus, config)?;
            v.manifest.algorithm = "reverse-bytes".into();
            v.test_vectors.clear();
            v.seal()?;
            self.load(v.clone())?;
            Ok(v)
        }
        fn load(&mut self, mut vocabulary: Vocabulary) -> kute::Result<()> {
            vocabulary.manifest.algorithm = "bytes".into();
            vocabulary.integrity = None;
            self.0 = Some(Engine::new().tokenizer(vocabulary)?);
            Ok(())
        }
        fn encode(&self, input: &CanonicalInput) -> kute::Result<TokenSequence> {
            let mut seq = self.0.as_ref().unwrap().encode(input)?;
            seq.ids.reverse();
            Ok(seq)
        }
        fn decode(&self, tokens: &TokenSequence) -> kute::Result<CanonicalInput> {
            let mut t = tokens.clone();
            t.ids.reverse();
            self.0.as_ref().unwrap().decode(&t)
        }
        fn vocabulary(&self) -> Option<&Vocabulary> {
            self.0.as_ref().and_then(|t| t.vocabulary())
        }
        fn capabilities(&self) -> TokenizerCapabilities {
            let mut c = self.0.as_ref().map(|t| t.capabilities()).unwrap_or_else(|| Engine::new().registry().tokenizer("bytes").unwrap().capabilities());
            c.algorithm = "reverse-bytes".into();
            c
        }
    }

    let mut engine = Engine::new();
    engine.registry_mut().register_algorithm("reverse-bytes", || Box::new(Reverse::default())).unwrap();
    // A name can be taken once; replacing is a separate, explicit call.
    assert!(engine.registry_mut().register_algorithm("reverse-bytes", || Box::new(Reverse::default())).is_err());
    assert!(engine.registry_mut().register_algorithm("bpe", || Box::new(Reverse::default())).is_err());
    engine.registry_mut().replace_algorithm("reverse-bytes", || Box::new(Reverse::default()));
    assert!(engine.registry().algorithm_names().contains(&"reverse-bytes".to_string()));
    let mut tokenizer = engine.registry().tokenizer("reverse-bytes").unwrap();
    tokenizer.train(&mut kute::VecCorpus::new(vec![]), &config("thirdparty:reverse", 256)).unwrap();
    let seq = tokenizer.encode(&CanonicalInput::bytes(Modality::Binary, *b"abc")).unwrap();
    assert_eq!(seq.ids, vec![99, 98, 97]);
    assert_eq!(tokenizer.decode(&seq).unwrap().payload, Payload::Bytes(b"abc".to_vec()));
    assert_eq!(tokenizer.capabilities().algorithm, "reverse-bytes");
}

/// Criterion: experimental HRM adapter contract exists.
#[test]
fn hrm_adapter_contract() {
    use kute::{Representation, RepresentationBackend};
    use kute_hrm::{inner_product, HrmPhaseMapping, MAPPING_ID};
    let engine = Engine::new();
    let vocab = text_vocab(&engine);
    let mapping = HrmPhaseMapping::new(256).unwrap();
    assert_eq!(mapping.mapping_id(), MAPPING_ID);
    let vector = |v: &Vocabulary, id: u32| match mapping.map(v, id).unwrap() {
        Representation::ComplexVector(c) => c,
        other => panic!("expected a complex vector, got {other:?}"),
    };
    let a = vector(&vocab, 300);
    assert_eq!(a.len(), 256);
    // Repeatable.
    assert_eq!(a, vector(&vocab, 300));
    // Unit energy; distinct tokens are close to orthogonal.
    let (norm, _) = inner_product(&a, &a).unwrap();
    assert!((norm - 1.0).abs() < 1e-4, "norm {norm}");
    let (re, im) = inner_product(&a, &vector(&vocab, 301)).unwrap();
    // Vectors from mappings of different dimensions have no inner product.
    assert!(inner_product(&a, &a[..255]).is_err());
    assert!((re * re + im * im).sqrt() < 0.3);
    // Content-addressed: the same payload maps to the same vector in another
    // vocabulary, whatever id it has there.
    let bytes = engine.train("bytes", "binary", &AdapterConfig::default(), &[], &config("test:hrm-bytes", 256)).unwrap();
    assert_eq!(vector(&vocab, 65), vector(&bytes, 65));
    assert!(mapping.map(&vocab, u32::MAX).is_err());
    assert!(HrmPhaseMapping::new(0).is_err());
    // The reference can be recorded in a vocabulary without changing any token.
    let mut with_ref = vocab.clone();
    with_ref.representations.push(mapping.reference());
    with_ref.seal().unwrap();
    assert!(with_ref.validate().is_ok());
    assert_eq!(with_ref.tokens, vocab.tokens);
}

/// Batch encoding is parallel and order-preserving.
#[test]
fn batch_encode_matches_single_encode() {
    let engine = Engine::new();
    let tokenizer = engine.tokenizer(text_vocab(&engine)).unwrap();
    let docs: Vec<Vec<u8>> = (0..64).map(|i| format!("{i}: {}", &TEXT[..40 + i]).into_bytes()).collect();
    let batch = engine.encode_batch(tokenizer.as_ref(), &docs).unwrap();
    for (doc, seq) in docs.iter().zip(&batch) {
        assert_eq!(seq, &engine.encode_raw(tokenizer.as_ref(), doc).unwrap());
    }
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_resumed_run_gives_the_vocabulary_of_one_run() {
    let engine = Engine::new();
    let mut docs: Vec<Vec<u8>> = Vec::new();
    for i in 0..400u32 {
        docs.push(format!("{TEXT} record {i} of the resumed run, {} and {}", i * 7 % 13, i * 31 % 17).into_bytes());
    }
    let train = |size: u32, from: Option<&Vocabulary>, limit: Option<usize>| {
        let mut c = config("test:resume", size);
        c.max_token_length = limit;
        c.resume_from = from.map(|v| Box::new(v.clone()));
        engine.train("bpe", "text", &AdapterConfig::default(), &docs, &c)
    };
    for limit in [None, Some(6)] {
        let whole = train(700, None, limit).unwrap();
        let first = train(400, None, limit).unwrap();
        assert!(first.merges.len() < whole.merges.len());
        let resumed = train(700, Some(&first), limit).unwrap();
        assert_eq!(resumed, whole);
        assert_eq!(resumed.digest().unwrap(), whole.digest().unwrap());
        // in two more steps, and from a vocabulary that is already complete
        let second = train(550, Some(&first), limit).unwrap();
        assert_eq!(train(700, Some(&second), limit).unwrap(), whole);
        assert_eq!(train(700, Some(&whole), limit).unwrap(), whole);
    }

    let first = train(400, None, None).unwrap();
    // a different corpus, different settings, a smaller target, another algorithm
    let mut c = config("test:resume", 700);
    c.resume_from = Some(Box::new(first.clone()));
    let other: Vec<Vec<u8>> = docs.iter().skip(1).cloned().collect();
    let err = engine.train("bpe", "text", &AdapterConfig::default(), &other, &c).unwrap_err();
    assert!(err.to_string().contains("different corpus"), "{err}");
    c.min_frequency = 3;
    let err = engine.train("bpe", "text", &AdapterConfig::default(), &docs, &c).unwrap_err();
    assert!(err.to_string().contains("min_frequency differs"), "{err}");
    assert!(train(300, Some(&first), None).is_err());
    let mut c = config("test:resume", 700);
    c.resume_from = Some(Box::new(first));
    assert!(engine.train("unigram", "text", &AdapterConfig::default(), &docs, &c).is_err());
}

#[test]
fn a_resumed_event_run_gives_the_vocabulary_of_one_run() {
    let engine = Engine::new();
    let names = ["START", "HEAT", "HOLD", "COOL", "DONE", "FAULT", "RESET"];
    let mut docs: Vec<Vec<u8>> = Vec::new();
    let mut state = 7u32;
    for _ in 0..120 {
        let mut text = String::new();
        for _ in 0..40 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let pick = (state >> 16) as usize % 10;
            let run: &[usize] = match pick {
                0..=3 => &[0, 1, 2, 3, 4],
                4..=5 => &[0, 1, 5, 6],
                6 => &[5, 6, 6],
                _ => &[pick % 7],
            };
            for &n in run {
                text.push_str(&format!("{{\"event\":\"{}\"}}\n", names[n]));
            }
        }
        docs.push(text.into_bytes());
    }
    let train = |size: u32, from: Option<&Vocabulary>| {
        let mut c = config("test:resume-events", size);
        c.pretokenizer = Pretokenizer::None;
        c.resume_from = from.map(|v| Box::new(v.clone()));
        engine.train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &docs, &c)
    };
    let whole = train(60, None).unwrap();
    let first = train(20, None).unwrap();
    assert!(first.merges.len() < whole.merges.len(), "{} {}", first.merges.len(), whole.merges.len());
    let resumed = train(60, Some(&first)).unwrap();
    assert_eq!(resumed, whole);
    assert_eq!(resumed.digest().unwrap(), whole.digest().unwrap());
    // a byte vocabulary is not a place to resume an event run from
    let mut c = config("test:resume-events", 300);
    let bytes = engine.train("bpe", "text", &AdapterConfig::default(), &docs, &c).unwrap();
    c.pretokenizer = Pretokenizer::None;
    c.resume_from = Some(Box::new(bytes));
    assert!(engine.train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &docs, &c).is_err());
}

/// Issue #2: a run checks its result even when no test vectors are stored.
#[test]
fn training_checks_its_result_without_storing_vectors() {
    /// The `bytes` baseline, except that decoding drops the last token.
    #[derive(Default)]
    struct Lossy(Option<Box<dyn Tokenizer>>);

    impl Tokenizer for Lossy {
        fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> kute::Result<Vocabulary> {
            let mut inner = Engine::new().registry().tokenizer("bytes")?;
            let v = inner.train(corpus, config)?;
            self.0 = Some(inner);
            Ok(v)
        }
        fn load(&mut self, vocabulary: Vocabulary) -> kute::Result<()> {
            self.0 = Some(Engine::new().tokenizer(vocabulary)?);
            Ok(())
        }
        fn encode(&self, input: &CanonicalInput) -> kute::Result<TokenSequence> {
            self.0.as_ref().unwrap().encode(input)
        }
        fn decode(&self, tokens: &TokenSequence) -> kute::Result<CanonicalInput> {
            let mut t = tokens.clone();
            t.ids.pop();
            self.0.as_ref().unwrap().decode(&t)
        }
        fn vocabulary(&self) -> Option<&Vocabulary> {
            self.0.as_ref().and_then(|t| t.vocabulary())
        }
        fn capabilities(&self) -> TokenizerCapabilities {
            Engine::new().registry().tokenizer("bytes").unwrap().capabilities()
        }
    }

    let mut engine = Engine::new();
    engine.registry_mut().register_algorithm("lossy", || Box::new(Lossy::default())).unwrap();
    let mut c = TrainingConfig::new(VocabularyId::parse("test:lossy").unwrap(), 256);
    assert!(!c.embed_test_vectors);
    c.pretokenizer = Pretokenizer::None;
    let docs = vec![TEXT.as_bytes().to_vec()];
    let err = engine.train("lossy", "binary", &AdapterConfig::default(), &docs, &c).unwrap_err();
    assert!(err.to_string().contains("does not reproduce the start of training record 1"), "{err}");
    // the honest algorithm passes, and nothing of the corpus is kept
    let v = engine.train("bytes", "binary", &AdapterConfig::default(), &docs, &c).unwrap();
    assert!(v.test_vectors.is_empty());
}

/// An added sequence must be reachable by the encoder even when the parent
/// already merges part of it (ADR-0021).
#[test]
fn an_evolved_sequence_is_used_where_the_parent_merges_inside_it() {
    let engine = Engine::new();
    // "HEAT HOLD" is by far the most common pair, so the parent merges it.
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(if i % 5 == 0 { concat!(r#"{"event":"START"}"#, "\n", r#"{"event":"HEAT"}"#, "\n", r#"{"event":"HOLD"}"#, "\n", r#"{"event":"DONE"}"#, "\n") } else { concat!(r#"{"event":"HEAT"}"#, "\n", r#"{"event":"HOLD"}"#, "\n") });
    }
    let docs = vec![text.into_bytes()];
    let mut c = config("test:evolve-inside", 5);
    c.pretokenizer = Pretokenizer::None;
    let parent = engine.train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &docs, &c).unwrap();
    let pattern = ["START", "HEAT", "HOLD"];
    let before = engine.tokenizer(parent.clone()).unwrap().encode(&CanonicalInput::symbols(pattern)).unwrap();
    assert_eq!(before.ids.len(), 2, "the parent joins HEAT HOLD and leaves START alone");
    let additions = vec![pattern.iter().map(|s| s.to_string()).collect::<Vec<_>>()];
    let child = kute::bpe::sequence::evolve(&parent, &additions, Version::parse("0.2.0").unwrap()).unwrap();
    assert_eq!(child.tokens.len(), parent.tokens.len() + 1);
    assert_eq!(&child.tokens[..parent.tokens.len()], &parent.tokens[..]);
    let tokenizer = engine.tokenizer(child).unwrap();
    let after = tokenizer.encode(&CanonicalInput::symbols(pattern)).unwrap();
    assert_eq!(after.ids.len(), 1);
    let back = tokenizer.decode(&after).unwrap();
    assert_eq!(back.payload, Payload::Symbols(pattern.iter().map(|s| s.to_string()).collect()));
}
