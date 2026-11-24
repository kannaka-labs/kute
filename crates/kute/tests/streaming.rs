//! Development directive v0.2, D02: training reads files one at a time, or
//! in pieces, without changing what is learned beyond where records end.

use kute::{AdapterConfig, Engine, KuteError, TrainingConfig, VocabularyId};
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kute-streaming-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(id: &str, size: u32, stream: Option<usize>) -> TrainingConfig {
    let mut c = TrainingConfig::new(VocabularyId::parse(id).unwrap(), size);
    c.stream_bytes = stream;
    c
}

fn payloads(v: &kute::Vocabulary) -> Vec<kute::TokenPayload> {
    v.tokens.iter().map(|t| t.payload.clone()).collect()
}

#[test]
fn event_training_is_the_same_whole_or_in_pieces() {
    let d = dir("events");
    let mut log = String::new();
    for i in 0..4000 {
        for e in ["OPEN", "FLOW", "CLOSE"] {
            log.push_str(&format!("{{\"event\":\"{e}\",\"n\":{i}}}\n"));
        }
        if i % 7 == 0 {
            log.push_str("{\"event\":\"ALARM\"}\n");
        }
    }
    std::fs::write(d.join("log.jsonl"), &log).unwrap();
    let engine = Engine::new();
    let paths = [d.join("log.jsonl")];
    let adapter = AdapterConfig::default();
    // A window boundary falls where a record ends, so pieces can shift
    // window boundaries. With a window longer than any piece's symbol count
    // would allow that; these events repeat with period 3 or 4, and the
    // learned tokens come out the same.
    let whole = engine.train_paths("sequence-bpe", "jsonl-events", &adapter, &paths, &config("test:stream:events", 12, None)).unwrap();
    let pieces = engine.train_paths("sequence-bpe", "jsonl-events", &adapter, &paths, &config("test:stream:events", 12, Some(4096))).unwrap();
    assert_eq!(whole.statistics["corpus_symbols"], pieces.statistics["corpus_symbols"]);
    assert_eq!(whole.statistics["corpus_records"], 1);
    assert!(pieces.statistics["corpus_records"] > 50, "the file was read in many pieces");
    assert_eq!(payloads(&whole)[..4], payloads(&pieces)[..4], "same alphabet");
    assert!(payloads(&pieces).contains(&kute::TokenPayload::Symbols(vec!["OPEN".into(), "FLOW".into(), "CLOSE".into()])));
    // The piece size is recorded, so the run can be repeated exactly.
    assert_eq!(pieces.provenance.training.as_ref().unwrap()["stream_bytes"], 4096);
    assert!(whole.provenance.training.as_ref().unwrap().get("stream_bytes").is_none());
    let again = engine.train_paths("sequence-bpe", "jsonl-events", &adapter, &paths, &config("test:stream:events", 12, Some(4096))).unwrap();
    assert_eq!(again.digest().unwrap(), pieces.digest().unwrap());
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn pieces_never_split_a_line_and_lose_nothing() {
    let d = dir("text");
    // Lines of very different lengths, one far longer than a piece.
    let mut text = String::new();
    for i in 0..3000 {
        text.push_str(&format!("line {i} the quick brown fox jumps over the lazy dog\n"));
        if i == 1500 {
            text.push_str(&"x".repeat(20_000));
            text.push('\n');
        }
    }
    text.push_str("last line without a line feed");
    std::fs::write(d.join("a.txt"), &text).unwrap();
    let engine = Engine::new();
    let paths = [d.join("a.txt")];
    let adapter = AdapterConfig::default();
    // With a pre-tokenizer, chunks are words and never cross a line feed,
    // so cutting after line feeds changes nothing that is counted. Without
    // one, input is counted in fixed windows that restart at each record,
    // and pieces would shift them.
    let with_pattern = |stream| {
        let mut c = config("test:stream:text", 400, stream);
        c.pretokenizer = kute::Pretokenizer::Gpt4;
        c
    };
    let whole = engine.train_paths("bpe", "text", &adapter, &paths, &with_pattern(None)).unwrap();
    let pieces = engine.train_paths("bpe", "text", &adapter, &paths, &with_pattern(Some(4096))).unwrap();
    assert_eq!(whole.statistics["unique_chunks"], pieces.statistics["unique_chunks"]);
    // Every byte was seen exactly once.
    assert_eq!(pieces.statistics["corpus_bytes"], text.len() as u64);
    assert_eq!(whole.statistics["corpus_bytes"], text.len() as u64);
    // Cutting only after line feeds leaves the pre-tokenizer's chunks alone
    // here, so the learned merges are identical.
    assert_eq!(payloads(&whole), payloads(&pieces));
    // Both encode and decode the text exactly.
    for v in [whole, pieces] {
        let t = engine.tokenizer(v).unwrap();
        let s = engine.encode_raw(t.as_ref(), text.as_bytes()).unwrap();
        assert_eq!(engine.decode_raw(t.as_ref(), &s).unwrap(), text.as_bytes());
    }
    // Binary input is cut at the size, wherever that falls.
    let blob: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(d.join("b.bin"), &blob).unwrap();
    let binary = engine.train_paths("bpe", "binary", &adapter, &[d.join("b.bin")], &config("test:stream:bin", 300, Some(4096))).unwrap();
    assert_eq!(binary.statistics["corpus_bytes"], 50_000);
    assert_eq!(binary.statistics["corpus_records"], 13);
    // Whole-document adapters and absurd sizes are refused.
    std::fs::write(d.join("c.json"), b"{\"a\":1}").unwrap();
    let refused = engine.train_paths("bpe", "json", &adapter, &[d.join("c.json")], &config("test:stream:json", 300, Some(4096)));
    assert!(matches!(refused, Err(KuteError::InvalidConfig(_))));
    assert!(config("test:stream:bad", 300, Some(16)).validate().is_err());
    std::fs::remove_dir_all(d).ok();
}
