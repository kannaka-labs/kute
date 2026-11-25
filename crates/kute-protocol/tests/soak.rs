//! AC-702, "aggregate peer memory and vocabulary budgets tested under
//! sustained load". One peer takes a long run of every kind of message a
//! stranger can send, from an application that reads its inbox only now and
//! then. Its budgets must hold at every step, and the memory of the process
//! must stop growing once the budgets are full.
//!
//! This file holds one test on purpose: the memory reading is for the whole
//! process, and a second test running beside it would be counted too.

use kute::{AdapterConfig, Engine, TrainingConfig, UnknownPolicy, Vocabulary, VocabularyId};
use kute_protocol::{ErrorCode, Message, Peer, Policy, VocabRef, EVOLUTION_RULE, PROTOCOL_VERSION};

fn events(names: &[String]) -> Vec<u8> {
    names.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect::<String>().into_bytes()
}

fn train(id: &str, names: &[String]) -> Vocabulary {
    let mut log = Vec::new();
    for _ in 0..40 {
        log.extend(events(names));
    }
    let config = TrainingConfig::new(VocabularyId::parse(id).unwrap(), 24);
    Engine::new().train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &[log], &config).unwrap()
}

/// A small deterministic generator, so a failure can be run again.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn budgets_hold_and_memory_stops_growing_over_a_long_mixed_run() {
    const STEPS: u64 = 300_000;
    const WARM: u64 = 60_000;

    let policy = Policy {
        accept_vocabularies: true,
        accept_proposals: true,
        give_vocabularies: true,
        answer_queries: true,
        translate: true,
        max_held_vocabularies: 8,
        max_inbox_messages: 16,
        max_inbox_bytes: 64 * 1024,
        replay_window: 64,
        ..Policy::default()
    };
    let limits = policy.clone();
    let mut peer = Peer::new("b", Engine::new(), policy);
    assert!(matches!(peer.handle(Message::Hello { protocol: PROTOCOL_VERSION, peer: "a".into(), software: "soak".into() })[0], Message::Hello { .. }));
    let capabilities = Peer::new("a", Engine::new(), Policy::default()).capabilities();
    assert!(matches!(peer.handle(capabilities)[0], Message::Capabilities { .. }));

    // More vocabularies than the peer may hold, so the count budget is
    // contested for the whole run.
    let offered: Vec<(Vocabulary, Vec<String>)> = (0..24)
        .map(|i| {
            let names: Vec<String> = (0..4).map(|k| format!("EVENT_{i}_{k}")).collect();
            (train(&format!("test:protocol:soak-{i}"), &names), names)
        })
        .collect();
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    let digests: Vec<String> = offered.iter().map(|(v, _)| sender.add_vocabulary(v.clone()).unwrap()).collect();

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut delivered, mut busy, mut refused, mut accepted) = (0u64, 0u64, 0u64, 0u64);
    let mut memory_after_warm_up = None;
    let mut proposal = 0u64;

    for step in 0..STEPS {
        let pick = (rng.next() % offered.len() as u64) as usize;
        let (vocabulary, names) = &offered[pick];
        let digest = &digests[pick];
        let replies = match rng.next() % 6 {
            // Offer a vocabulary, and send it if asked.
            0 => {
                let mut replies = peer.handle(Message::VocabAnnounce { vocabulary: VocabRef::of(vocabulary).unwrap() });
                if matches!(replies[0], Message::VocabRequest { .. }) {
                    replies = peer.handle(Message::VocabData { digest: digest.clone(), kvf: kute::vocab::to_vec(vocabulary).unwrap() });
                }
                replies
            }
            // A message, under a fresh id or a recently used one.
            1 | 2 => {
                let id = if rng.next() % 4 == 0 { step.saturating_sub(rng.next() % 200) } else { step } + 1;
                let (message, _) = sender.encode_message(id, digest, &events(names), UnknownPolicy::Strict).unwrap();
                peer.handle(message)
            }
            // Ask for an extension of something it may hold.
            3 => {
                proposal += 1;
                peer.handle(Message::SymbolPropose {
                    digest: digest.clone(),
                    additions: vec![vec![format!("NEW_{proposal}")]],
                    version: format!("0.{}.0", proposal + 1),
                    rule: EVOLUTION_RULE.into(),
                    signatures: vec![],
                })
            }
            4 => peer.handle(Message::SymbolQuery { digest: digest.clone(), symbols: vec![names[0].clone()] }),
            _ => peer.handle(Message::VocabRequest { digest: digest.clone() }),
        };
        match &replies[0] {
            Message::Ack { .. } => delivered += 1,
            Message::VocabAccept { .. } => accepted += 1,
            Message::Error { code: ErrorCode::Busy, .. } => busy += 1,
            Message::Error { .. } => refused += 1,
            _ => {}
        }

        // The budgets, at every step.
        let held = peer.digests();
        assert!(held.len() as u64 <= limits.max_held_vocabularies, "step {step}: {} vocabularies", held.len());
        let tokens: u64 = held.iter().map(|d| peer.vocabulary(d).unwrap().len() as u64).sum();
        assert!(tokens <= limits.max_held_tokens, "step {step}: {tokens} tokens");

        // The application reads rarely.
        if rng.next() % 500 == 0 {
            let inbox = peer.take_inbox();
            assert!(inbox.len() as u64 <= limits.max_inbox_messages, "step {step}: {} in the inbox", inbox.len());
            let bytes: u64 = inbox.iter().map(|d| d.bytes.len() as u64).sum();
            assert!(bytes <= limits.max_inbox_bytes, "step {step}: {bytes} inbox bytes");
        }
        if step == WARM {
            memory_after_warm_up = kute_bench::peak_memory_bytes();
        }
    }

    // Every kind of outcome happened, so the run exercised the limits and
    // not only the refusals.
    assert!(delivered > 1_000 && busy > 1_000 && refused > 1_000 && accepted > 1_000, "delivered {delivered}, busy {busy}, refused {refused}, accepted {accepted}");
    assert_eq!(peer.digests().len() as u64, limits.max_held_vocabularies);

    // Peak memory of the process is a high-water mark: if it rose between
    // the warm-up and the end, something kept growing. Allocators round and
    // reuse, so a small margin is allowed; a leak of one message per step
    // over 240,000 steps would be far past it.
    if let (Some(before), Some(after)) = (memory_after_warm_up, kute_bench::peak_memory_bytes()) {
        let grown = after.saturating_sub(before);
        assert!(grown < 8 * 1024 * 1024, "peak memory grew by {grown} bytes after warm-up ({before} to {after})");
        eprintln!("soak: {STEPS} steps; delivered {delivered}, busy {busy}, refused {refused}, accepted {accepted}; peak memory {before} at warm-up, {after} at the end");
    }
}
