//! Gate AC-712 of the v0.7 specification: property tests over the parsers
//! that read untrusted bytes. Valid files are damaged in many ways (bits
//! flipped, cut, bytes inserted, lengths forged, parts swapped between
//! files) and each parser must answer with a value or an error: no panic,
//! no hang, no allocation sized by a number in the input. Whatever parses
//! must also be usable: a vocabulary that loads must load into a tokenizer
//! or be refused there, and anything a tokenizer accepts must decode
//! without panicking.
//!
//! The damage is drawn from a fixed seed, so a failure repeats.

use kute::{vocab, AdapterConfig, Engine, TokenSequence, TrainingConfig, UnknownPolicy, Vocabulary, VocabularyId};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One damaged copy of `good`. `other` is a second valid file of the same
/// kind, to splice from.
fn damage(rng: &mut Rng, good: &[u8], other: &[u8]) -> Vec<u8> {
    let mut bytes = good.to_vec();
    for _ in 0..1 + rng.below(3) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(9) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes[at] = rng.next() as u8,
            2 => bytes.truncate(at),
            3 => {
                bytes.insert(at, rng.next() as u8);
            }
            4 => {
                bytes.remove(at);
            }
            // A forged length or count: the largest values, where they hurt.
            5 => {
                let fill = [0xff, 0x7f, 0x80, 0x00][rng.below(4)];
                for b in bytes.iter_mut().skip(at).take(1 + rng.below(8)) {
                    *b = fill;
                }
            }
            // A run of the same byte, to make deep or long structures.
            6 => {
                let run = vec![[b'[', b'{', b'"', b'9', 0u8][rng.below(5)]; 1 + rng.below(2000)];
                bytes.splice(at..at, run);
            }
            // The tail of another valid file.
            7 if !other.is_empty() => {
                let from = rng.below(other.len());
                bytes.truncate(at);
                bytes.extend_from_slice(&other[from..]);
            }
            // A block repeated.
            _ => {
                let end = (at + 1 + rng.below(64)).min(bytes.len());
                let block = bytes[at..end].to_vec();
                bytes.splice(at..at, block);
            }
        }
    }
    bytes
}

fn events(names: &[&str], repeat: usize) -> Vec<u8> {
    names.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect::<String>().repeat(repeat).into_bytes()
}

const TEXT: &str = "The engine reads the event log. The engine writes the event log. Naïve café, 日本語, 🙂.\n";

fn vocabularies(engine: &Engine) -> Vec<Vocabulary> {
    let none = AdapterConfig::default();
    let mut embed = TrainingConfig::new(VocabularyId::parse("fuzz:text").unwrap(), 300);
    embed.embed_test_vectors = true;
    vec![
        engine.train("bpe", "text", &none, &[TEXT.repeat(4).into_bytes()], &embed).unwrap(),
        engine.train("sequence-bpe", "jsonl-events", &none, &[events(&["A", "B", "A", "C"], 30)], &TrainingConfig::new(VocabularyId::parse("fuzz:events").unwrap(), 12)).unwrap(),
        engine.train("dictionary", "jsonl-events", &none, &[events(&["A", "B", "C"], 5)], &TrainingConfig::new(VocabularyId::parse("fuzz:dict").unwrap(), 8)).unwrap(),
        engine.train("char", "text", &none, &[TEXT.as_bytes().to_vec()], &TrainingConfig::new(VocabularyId::parse("fuzz:char").unwrap(), 300)).unwrap(),
    ]
}

/// A vocabulary that parsed must either be refused by the tokenizer or
/// work: encode something and decode it back without panicking.
fn exercise(engine: &Engine, vocabulary: Vocabulary) -> bool {
    let Ok(tokenizer) = engine.tokenizer(vocabulary) else { return false };
    for raw in [TEXT.as_bytes().to_vec(), events(&["A", "B", "Z"], 2), Vec::new(), vec![0xff, 0x00, 0x80]] {
        if let Ok(sequence) = engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Escape) {
            let _ = engine.decode_raw_bounded(tokenizer.as_ref(), &sequence, 1 << 20);
        }
    }
    let _ = engine.verify(tokenizer.vocabulary().unwrap());
    true
}

#[test]
fn damaged_vocabulary_files_are_refused_or_usable() {
    let engine = Engine::new();
    let good = vocabularies(&engine);
    let mut files: Vec<Vec<u8>> = Vec::new();
    for v in &good {
        files.push(vocab::to_vec(v).unwrap());
        files.push(vocab::to_vec_compact(v).unwrap());
        // The digest catches accidents, not intent: anyone can damage the
        // content and leave the digest out, and writing the result gives it
        // a valid one. Without these files nearly every damaged file stops
        // at the digest and nothing behind it is tested.
        let mut unsealed: serde_json::Value = serde_json::from_slice(&vocab::to_vec(v).unwrap()).unwrap();
        unsealed.as_object_mut().unwrap().remove("integrity").unwrap();
        files.push(serde_json::to_vec(&unsealed).unwrap());
        files.push(serde_json::to_vec_pretty(&unsealed).unwrap());
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut parsed, mut loaded) = (0u32, 0u32);
    let rounds = 8000;
    for round in 0..rounds {
        let good = &files[round % files.len()];
        let other = &files[rng.below(files.len())];
        let bytes = damage(&mut rng, good, other);
        if let Ok(vocabulary) = vocab::from_slice(&bytes) {
            parsed += 1;
            // What parses writes back, and that reads and writes back the same.
            // (Writing fills in the digest when the file had none, so compare
            // what is written, not the values.)
            let written = vocab::to_vec(&vocabulary).unwrap();
            let again = vocab::from_slice(&written).unwrap();
            assert_eq!(vocab::to_vec(&again).unwrap(), written, "round {round}");
            // The resealed value is the one with a digest that matches.
            if exercise(&engine, again) {
                loaded += 1;
            }
        }
    }
    // The damage must not be so heavy that nothing ever gets through to the
    // later stages, or those stages were not tested.
    assert!(parsed > 100 && loaded > 50, "of {rounds} damaged files {parsed} parsed and {loaded} loaded");
    println!("vocabulary files: {rounds} damaged, {parsed} parsed, {loaded} loaded into a tokenizer");
}

#[test]
fn damaged_token_containers_are_refused_or_decodable() {
    let engine = Engine::new();
    let good = vocabularies(&engine);
    let mut tokenizers = Vec::new();
    let mut files: Vec<Vec<u8>> = Vec::new();
    for v in &good {
        let tokenizer = engine.tokenizer(v.clone()).unwrap();
        for raw in [TEXT.repeat(3).into_bytes(), events(&["A", "B", "A", "C", "NEW"], 6)] {
            if let Ok(sequence) = engine.encode_raw_open(tokenizer.as_ref(), &raw, UnknownPolicy::Escape) {
                files.push(sequence.to_bytes().unwrap());
                files.push(sequence.to_bytes_compressed().unwrap());
            }
        }
        tokenizers.push(tokenizer);
    }
    assert!(files.len() >= 8);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut parsed, mut decoded) = (0u32, 0u32);
    let rounds = 12000;
    for round in 0..rounds {
        let good = &files[round % files.len()];
        let other = &files[rng.below(files.len())];
        let bytes = damage(&mut rng, good, other);
        let Ok(sequence) = TokenSequence::from_bytes(&bytes) else { continue };
        parsed += 1;
        // It still names some vocabulary; under every one we hold, decoding
        // is bounded and either works or is an error.
        for tokenizer in &tokenizers {
            if engine.decode_raw_bounded(tokenizer.as_ref(), &sequence, 1 << 20).is_ok() {
                decoded += 1;
            }
        }
        // What parses writes back and reads again as the same value.
        if let Ok(written) = sequence.to_bytes() {
            assert_eq!(TokenSequence::from_bytes(&written).unwrap(), sequence, "round {round}");
        }
    }
    assert!(parsed > 20, "only {parsed} of {rounds} damaged containers parsed");
    println!("containers: {rounds} damaged, {parsed} parsed, {decoded} decoded under some vocabulary");
}

/// The JSON adapters read whatever a user or a peer hands them.
#[test]
fn damaged_structured_input_is_refused_or_canonical() {
    let engine = Engine::new();
    let good: Vec<Vec<u8>> = vec![
        r#"{"a":1,"b":[1,2.5,-0,1e3,{"c":null,"d":"x\"y\\zé"}],"e":true,"f":18446744073709551615}"#.as_bytes().to_vec(),
        br#"[{"event":"A","t":1.25},{"event":"B","t":-3},"s",[],{}]"#.to_vec(),
        events(&["A", "B", "C"], 3),
    ];
    let mut rng = Rng(0x243f_6a88_85a3_08d3);
    let mut accepted = 0u32;
    let rounds = 20000;
    for round in 0..rounds {
        let other = rng.below(good.len());
        let bytes = damage(&mut rng, &good[round % good.len()], &good[other]);
        for adapter in ["json", "jsonl", "jsonl-events"] {
            for strict in ["true", "false"] {
                let config = AdapterConfig::default().with("strict", strict);
                let adapter = engine.registry().adapter(adapter).unwrap();
                let Ok(canonical) = adapter.ingest(&bytes, &config) else { continue };
                accepted += 1;
                // Canonical form is a fixed point: reading what was written
                // gives the same thing, and strict mode accepts it.
                if let Ok(written) = adapter.reconstruct(&canonical, &config) {
                    let strictly = AdapterConfig::default();
                    let again = adapter.ingest(&written, &strictly).unwrap_or_else(|e| panic!("round {round}: canonical output refused: {e}"));
                    assert_eq!(again, canonical, "round {round}");
                }
            }
        }
    }
    assert!(accepted > 100, "only {accepted} inputs were accepted");
    println!("structured input: {rounds} damaged, {accepted} accepted by some adapter and mode");
}
