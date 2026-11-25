//! A signed proposal against the real signature check: what a holder of
//! one genuine signature can and cannot get a peer to take.

use kute::{AdapterConfig, Engine, TrainingConfig, Version, Vocabulary, VocabularyId};
use kute_protocol::{ErrorCode, Message, Peer, Policy, VocabRef, EVOLUTION_RULE, PROTOCOL_VERSION};

fn train() -> Vocabulary {
    let mut log = String::new();
    for _ in 0..40 {
        for name in ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"] {
            log.push_str(&format!("{{\"event\":\"{name}\"}}\n"));
        }
    }
    let config = TrainingConfig::new(VocabularyId::parse("test:proposal:attacks").unwrap(), 24);
    Engine::new().train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &[log.into_bytes()], &config).unwrap()
}

fn key(dir: &std::path::Path, name: &str) -> (std::path::PathBuf, String) {
    let path = dir.join(format!("{name}.pem"));
    let made = kute_cli::sign::keygen(&path).unwrap();
    (path, made["public_key"].as_str().unwrap().to_string())
}

/// The signatures `key` makes for the child of `parent` under these additions and version.
fn vouch(parent: &Vocabulary, symbol: &str, version: &str, key: &std::path::Path) -> (String, Vec<kute::Signature>) {
    let mut child = kute::bpe::sequence::evolve(parent, &[vec![symbol.to_string()]], Version::parse(version).unwrap()).unwrap();
    child.seal().unwrap();
    kute_cli::sign::signer(key).unwrap()(&mut child).unwrap();
    let integrity = child.integrity.unwrap();
    (integrity.digest, integrity.signatures)
}

fn peer(parent: &Vocabulary, trusted: Option<&str>) -> Peer {
    let mut peer = Peer::new("b", Engine::new(), Policy { accept_vocabularies: true, accept_proposals: true, answer_queries: true, ..Policy::default() });
    assert!(matches!(peer.handle(Message::Hello { protocol: PROTOCOL_VERSION, peer: "a".into(), software: "test".into() })[0], Message::Hello { .. }));
    let caps = Peer::new("a", Engine::new(), Policy::default()).capabilities();
    assert!(matches!(peer.handle(caps)[0], Message::Capabilities { .. }));
    peer.add_vocabulary(parent.clone()).unwrap();
    peer.handle(Message::VocabAnnounce { vocabulary: VocabRef::of(parent).unwrap() });
    if let Some(trusted) = trusted {
        peer.require_vocabulary_check(Box::new(kute_cli::sign::gate(&[trusted.to_string()]).unwrap()));
    }
    peer
}

fn refused(replies: &[Message]) -> Option<ErrorCode> {
    match &replies[0] {
        Message::Error { code, .. } => Some(*code),
        _ => None,
    }
}

#[test]
fn a_genuine_signature_passes_only_for_the_content_it_was_made_for() {
    let dir = std::env::temp_dir().join(format!("kute-proposal-attacks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let parent = train();
    let digest = parent.compute_digest().unwrap();
    let (queen, queen_public) = key(&dir, "queen");
    let (stranger, _) = key(&dir, "stranger");
    let propose = |symbol: &str, version: &str, signatures: Vec<kute::Signature>| Message::SymbolPropose {
        digest: digest.clone(),
        additions: vec![vec![symbol.to_string()]],
        version: version.into(),
        rule: EVOLUTION_RULE.into(),
        signatures,
    };

    // What an eavesdropper has: the queen's signature of one child.
    let (child, genuine) = vouch(&parent, "VALVE_STUCK", "0.2.0", &queen);
    let (_, strangers) = vouch(&parent, "VALVE_STUCK", "0.2.0", &stranger);

    let mut strict = peer(&parent, Some(&queen_public));
    // Other additions, another version, the parent itself in the id: the queen's signature does not carry over.
    for (symbol, version) in [("VALVE_OPEN", "0.2.0"), ("VALVE_STUCK", "0.3.0"), ("VALVE_STUCK", "0.2.1")] {
        assert_eq!(refused(&strict.handle(propose(symbol, version, genuine.clone()))), Some(ErrorCode::Refused), "{symbol} {version}");
    }
    // A stranger's signature under the queen's name; the queen's bytes under another algorithm name; a damaged one.
    let mut renamed = strangers.clone();
    renamed[0].key_id = genuine[0].key_id.clone();
    let mut other_algorithm = genuine.clone();
    other_algorithm[0].algorithm = "none".into();
    let mut damaged = genuine.clone();
    damaged[0].signature = damaged[0].signature.chars().rev().collect();
    for (name, signatures) in [("stranger", strangers.clone()), ("renamed", renamed), ("algorithm", other_algorithm), ("damaged", damaged)] {
        assert_eq!(refused(&strict.handle(propose("VALVE_STUCK", "0.2.0", signatures))), Some(ErrorCode::Refused), "{name}");
    }
    assert!(strict.vocabulary(&child).is_none(), "nothing refused is held");
    assert_eq!(strict.digests().len(), 1);

    // The genuine one among strangers' is taken, and replaying it is taken again: it is the same content.
    let mut mixed = strangers.clone();
    mixed.extend(genuine.clone());
    assert!(matches!(&strict.handle(propose("VALVE_STUCK", "0.2.0", mixed))[0], Message::VocabAccept { digest } if *digest == child));
    assert!(matches!(&strict.handle(propose("VALVE_STUCK", "0.2.0", genuine.clone()))[0], Message::VocabAccept { .. }));
    // What is held afterwards is the last copy that passed, here with the queen's signature alone.
    let held = &strict.vocabulary(&child).unwrap().integrity.as_ref().unwrap().signatures;
    assert_eq!((held.len(), &held[0].key_id), (1, &genuine[0].key_id));

    // A peer with no check attaches what it is given without looking: the held copy carries the
    // signatures of whoever proposed last, so they mean nothing until someone verifies them.
    let mut lax = peer(&parent, None);
    assert!(matches!(&lax.handle(propose("VALVE_STUCK", "0.2.0", strangers.clone()))[0], Message::VocabAccept { .. }));
    assert!(matches!(&lax.handle(propose("VALVE_STUCK", "0.2.0", genuine.clone()))[0], Message::VocabAccept { .. }));
    let held = lax.vocabulary(&child).unwrap().integrity.as_ref().unwrap().signatures.clone();
    assert_eq!((held.len(), &held[0].key_id), (1, &genuine[0].key_id), "the later proposal's signatures replace the earlier one's");
    let _ = std::fs::remove_dir_all(&dir);
}
