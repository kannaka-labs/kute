//! Gate from the development directive v0.2: "Two independently configured
//! peers negotiate a vocabulary, reject mismatches and exchange encoded
//! messages."

use kute::{AdapterConfig, Engine, KuteError, TrainingConfig, UnknownPolicy, Version, Vocabulary, VocabularyId};
use kute_protocol::{ErrorCode, Framed, Message, Peer, Policy, Session, Transport, VocabRef, DEFAULT_MAX_FRAME, EVOLUTION_RULE, PROTOCOL_VERSION};
use std::net::{TcpListener, TcpStream};

fn events(names: &[&str]) -> Vec<u8> {
    names.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect::<String>().into_bytes()
}

fn train(id: &str, cycle: &[&str]) -> Vocabulary {
    let mut log = Vec::new();
    for _ in 0..40 {
        log.extend(events(cycle));
    }
    let config = TrainingConfig::new(VocabularyId::parse(id).unwrap(), 24);
    Engine::new().train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &[log], &config).unwrap()
}

const CYCLE: [&str; 4] = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];

/// The default policy takes nothing from the other side; most tests need a
/// peer that takes vocabularies.
fn open() -> Policy {
    Policy { accept_vocabularies: true, ..Policy::default() }
}

/// A responder on a real socket, in its own thread, with its own engine and
/// policy. Returns the peer after the initiator disconnects.
fn responder(policy: Policy, held: Vec<Vocabulary>) -> (std::net::SocketAddr, std::thread::JoinHandle<Peer>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut peer = Peer::new("plant-b", Engine::new(), policy);
        for v in held {
            peer.add_vocabulary(v).unwrap();
        }
        let (stream, _) = listener.accept().unwrap();
        let mut session = Session::new(peer, Framed::new(stream, DEFAULT_MAX_FRAME));
        session.serve().unwrap();
        session.peer
    });
    (address, handle)
}

fn initiator(address: std::net::SocketAddr, held: &[Vocabulary]) -> (Session<Framed<TcpStream>>, Vec<String>) {
    let mut peer = Peer::new("plant-a", Engine::new(), Policy::default());
    let digests = held.iter().map(|v| peer.add_vocabulary(v.clone()).unwrap()).collect();
    (Session::new(peer, Framed::new(TcpStream::connect(address).unwrap(), DEFAULT_MAX_FRAME)), digests)
}

#[test]
fn peers_negotiate_exchange_and_extend_a_vocabulary() {
    let vocabulary = train("test:protocol:events", &CYCLE);
    let (address, handle) = responder(Policy { accept_proposals: true, answer_queries: true, translate: true, ..open() }, Vec::new());
    let (mut a, digests) = initiator(address, std::slice::from_ref(&vocabulary));
    let digest = &digests[0];

    let remote = a.connect().unwrap();
    assert_eq!(remote.peer, "plant-b");
    assert!(remote.algorithms.contains(&"sequence-bpe".to_string()));

    // B has never seen the vocabulary: it is transferred and verified once.
    assert!(a.share(digest).unwrap(), "first share transfers the vocabulary");
    assert!(!a.share(digest).unwrap(), "second share finds it already there");

    // An encoded message arrives and decodes to what A's own decoder gives.
    let raw = events(&["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK", "MOTOR_START", "CURRENT_HIGH"]);
    assert_eq!(a.send(digest, &raw, UnknownPolicy::Strict).unwrap(), 6);

    // A new event appears. Strict refuses it locally; escape carries it.
    let novel = events(&["MOTOR_START", "VALVE_7_STUCK", "VALVE_7_STUCK", "OPERATOR_ACK"]);
    assert!(matches!(a.send(digest, &novel, UnknownPolicy::Strict), Err(KuteError::UnknownSymbol(_))));
    assert_eq!(a.send(digest, &novel, UnknownPolicy::Escape).unwrap(), 4);

    // B does not know the symbol as a token yet.
    let stuck = vec!["VALVE_7_STUCK".to_string()];
    assert_eq!(a.query(digest, &stuck).unwrap(), None);
    assert!(a.query(digest, &["MOTOR_START".to_string()]).unwrap().is_some());

    // A proposes it, with a two-event sequence; both sides derive the same
    // next version without sending the vocabulary again.
    let additions = vec![stuck.clone(), vec!["VALVE_7_STUCK".to_string(), "VALVE_7_STUCK".to_string()]];
    let next = a.propose(digest, &additions, "0.2.0").unwrap();
    assert_ne!(&next, digest);
    let id = a.query(&next, &stuck).unwrap().expect("the symbol is a token now");
    assert_eq!(id as usize, vocabulary.len(), "old ids are unchanged; the new token is appended");
    assert_eq!(a.send(&next, &novel, UnknownPolicy::Strict).unwrap(), 4, "the evolved vocabulary encodes it strictly");

    // The same step proposed again is refused and leaves both sides alone.
    assert!(a.propose(&next, &additions, "0.3.0").is_err());

    // Translation: B re-encodes an old-version sequence under the new one.
    let old = a.peer.engine().encode_raw_open(
        a.peer.engine().tokenizer(vocabulary.clone()).unwrap().as_ref(),
        &novel,
        UnknownPolicy::Escape,
    ).unwrap();
    assert_eq!(old.escapes.len(), 1);
    let translated = a.translate(&old, &next).unwrap();
    assert!(translated.escapes.is_empty(), "the new vocabulary needs no escapes");
    assert!(translated.ids.len() < old.ids.len());

    drop(a);
    let mut b = handle.join().unwrap();
    assert_eq!(b.digests().len(), 2);
    assert_eq!(b.vocabulary(&next).unwrap().version(), &Version::new(0, 2, 0));
    let inbox = b.take_inbox();
    assert_eq!(inbox.len(), 3);
    assert_eq!(inbox[0].bytes, raw);
    assert_eq!(inbox[1].bytes, novel);
    assert_eq!(inbox[2].bytes, novel);
}

#[test]
fn mismatches_are_rejected() {
    // B holds a vocabulary with the same id and version as A's, different content.
    let ours = train("test:protocol:conflict", &CYCLE);
    let theirs = train("test:protocol:conflict", &["DOOR_OPEN", "DOOR_CLOSE", "LOCK"]);
    assert_ne!(ours.digest().unwrap(), theirs.digest().unwrap());
    let (address, handle) = responder(Policy::default(), vec![theirs]);
    let (mut a, digests) = initiator(address, std::slice::from_ref(&ours));
    a.connect().unwrap();
    let conflict = a.share(&digests[0]).unwrap_err().to_string();
    assert!(conflict.contains("VocabularyConflict"), "{conflict}");
    // A message under a vocabulary B does not hold is refused, not guessed at.
    let unknown = a.send(&digests[0], &events(&CYCLE), UnknownPolicy::Strict).unwrap_err().to_string();
    assert!(unknown.contains("UnknownVocabulary"), "{unknown}");
    // B's policy does not accept proposals.
    drop(a);
    let mut b = handle.join().unwrap();
    assert!(b.take_inbox().is_empty());

    let (address, handle) = responder(Policy { accept_vocabularies: false, ..Policy::default() }, vec![ours.clone()]);
    let (mut a, digests) = initiator(address, &[ours, train("test:protocol:other", &CYCLE)]);
    a.connect().unwrap();
    assert!(!a.share(&digests[0]).unwrap(), "a vocabulary both sides hold needs no transfer");
    assert!(a.share(&digests[1]).unwrap_err().to_string().contains("Refused"));
    assert!(a.propose(&digests[0], &[vec!["X".to_string()]], "0.2.0").unwrap_err().to_string().contains("Refused"));
    assert_eq!(a.peer.digests().len(), 2, "a refused proposal changes nothing locally");
    drop(a);
    handle.join().unwrap();
}

fn ready_peer(policy: Policy) -> Peer {
    let mut peer = Peer::new("b", Engine::new(), policy);
    assert!(matches!(peer.handle(Message::Hello { protocol: PROTOCOL_VERSION, peer: "a".into(), software: "test".into() })[0], Message::Hello { .. }));
    let caps = Peer::new("a", Engine::new(), Policy::default()).capabilities();
    assert!(matches!(peer.handle(caps)[0], Message::Capabilities { .. }));
    peer
}

fn code(replies: &[Message]) -> ErrorCode {
    match &replies[0] {
        Message::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn a_peer_trusts_nothing_it_is_sent() {
    // Order is enforced.
    let mut fresh = Peer::new("b", Engine::new(), Policy::default());
    assert_eq!(code(&fresh.handle(Message::VocabRequest { digest: "sha256:00".into() })), ErrorCode::OutOfOrder);
    assert_eq!(code(&fresh.handle(Message::Hello { protocol: 99, peer: "a".into(), software: "x".into() })), ErrorCode::UnsupportedProtocol);

    let vocabulary = train("test:protocol:hostile", &CYCLE);
    let honest = VocabRef::of(&vocabulary).unwrap();
    let kvf = kute::vocab::to_vec(&vocabulary).unwrap();

    // Data nobody asked for.
    let mut peer = ready_peer(open());
    assert_eq!(code(&peer.handle(Message::VocabData { digest: honest.digest.clone(), kvf: kvf.clone() })), ErrorCode::OutOfOrder);

    // Announce one vocabulary, send another.
    let other = train("test:protocol:hostile-other", &["A", "B", "C"]);
    assert!(matches!(peer.handle(Message::VocabAnnounce { vocabulary: honest.clone() })[0], Message::VocabRequest { .. }));
    let swapped = Message::VocabData { digest: honest.digest.clone(), kvf: kute::vocab::to_vec(&other).unwrap() };
    assert_eq!(code(&peer.handle(swapped)), ErrorCode::Invalid);
    assert!(peer.digests().is_empty());

    // A vocabulary altered in transit: the recorded digest no longer matches.
    assert!(matches!(peer.handle(Message::VocabAnnounce { vocabulary: honest.clone() })[0], Message::VocabRequest { .. }));
    let tampered = String::from_utf8(kvf.clone()).unwrap().replacen("MOTOR_TRIP", "MOTOR_TRAP", 1).into_bytes();
    assert_eq!(code(&peer.handle(Message::VocabData { digest: honest.digest.clone(), kvf: tampered })), ErrorCode::Invalid);
    assert!(peer.digests().is_empty());

    // An algorithm this peer does not have, and a vocabulary over its size limit.
    let mut alien = honest.clone();
    alien.algorithm = "not-an-algorithm".into();
    alien.digest = "sha256:11".into();
    assert_eq!(code(&peer.handle(Message::VocabAnnounce { vocabulary: alien })), ErrorCode::UnsupportedAlgorithm);
    let mut small = ready_peer(Policy { max_vocabulary_tokens: 4, ..open() });
    assert_eq!(code(&small.handle(Message::VocabAnnounce { vocabulary: honest.clone() })), ErrorCode::TooLarge);

    // The honest transfer works, and then a message that would decode past
    // the budget is refused before decoding.
    let mut tight = ready_peer(Policy { max_decoded_units: 8, ..open() });
    assert!(matches!(tight.handle(Message::VocabAnnounce { vocabulary: honest.clone() })[0], Message::VocabRequest { .. }));
    assert_eq!(tight.handle(Message::VocabData { digest: honest.digest.clone(), kvf })[0], Message::VocabAccept { digest: honest.digest.clone() });
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    sender.add_vocabulary(vocabulary).unwrap();
    let (long, _) = sender.encode_message(1, &honest.digest, &events(&CYCLE.repeat(5)), UnknownPolicy::Strict).unwrap();
    assert_eq!(code(&tight.handle(long)), ErrorCode::TooLarge);
    let (short, expected) = sender.encode_message(2, &honest.digest, &events(&CYCLE), UnknownPolicy::Strict).unwrap();
    assert_eq!(tight.handle(short)[0], Message::Ack { id: 2, units: 4, sha256: expected });
    // Garbage where a container should be.
    assert_eq!(code(&tight.handle(Message::EncodedMessage { id: 3, container: b"KUTE\x01garbage".to_vec() })), ErrorCode::Invalid);
    // Replies are not requests.
    assert_eq!(code(&tight.handle(Message::Ack { id: 1, units: 0, sha256: String::new() })), ErrorCode::OutOfOrder);
}

#[test]
fn framing_is_bounded() {
    // A forged length is refused before anything is allocated for it.
    let mut forged = (u32::MAX).to_le_bytes().to_vec();
    forged.extend_from_slice(b"{}");
    let mut framed = Framed::new(std::io::Cursor::new(forged), 1024);
    assert!(matches!(framed.recv(), Err(KuteError::LimitExceeded(_))));
    // A frame cut short, bytes that are not a message, and a clean end.
    let mut cut = 100u32.to_le_bytes().to_vec();
    cut.extend_from_slice(b"{\"type\":");
    assert!(Framed::new(std::io::Cursor::new(cut), 1024).recv().is_err());
    let mut junk = 2u32.to_le_bytes().to_vec();
    junk.extend_from_slice(b"{}");
    assert!(Framed::new(std::io::Cursor::new(junk), 1024).recv().is_err());
    assert_eq!(Framed::new(std::io::Cursor::new(Vec::new()), 1024).recv().unwrap(), None);
    // What is written is what is read.
    let mut pipe = Framed::new(std::io::Cursor::new(Vec::new()), 1024);
    let message = Message::Ack { id: 7, units: 3, sha256: "ab".into() };
    pipe.send(&message).unwrap();
    assert!(pipe.send(&Message::EncodedMessage { id: 1, container: vec![0; 4096] }).is_err(), "over the frame limit");
}

/// Hand `vocabulary` to a ready peer the way the other side would.
fn give(peer: &mut Peer, vocabulary: &Vocabulary) -> Vec<Message> {
    let announced = VocabRef::of(vocabulary).unwrap();
    let reply = peer.handle(Message::VocabAnnounce { vocabulary: announced.clone() });
    if !matches!(reply[0], Message::VocabRequest { .. }) {
        return reply;
    }
    peer.handle(Message::VocabData { digest: announced.digest, kvf: kute::vocab::to_vec(vocabulary).unwrap() })
}

/// B2: with no policy set, the other side can add nothing.
#[test]
fn the_default_policy_takes_nothing_from_the_other_side() {
    let vocabulary = train("test:protocol:deny", &CYCLE);
    let mut peer = ready_peer(Policy::default());
    assert_eq!(code(&give(&mut peer, &vocabulary)), ErrorCode::Refused);
    assert!(peer.digests().is_empty());
    let mut holder = ready_peer(Policy::default());
    let digest = holder.add_vocabulary(vocabulary).unwrap();
    let propose = Message::SymbolPropose { digest, additions: vec![vec!["X".into()]], version: "0.2.0".into(), rule: EVOLUTION_RULE.into() };
    assert_eq!(code(&holder.handle(propose)), ErrorCode::Refused);
    assert_eq!(holder.digests().len(), 1);
}

/// AC-703: a proposal made under another evolution rule is refused before
/// anything is derived, and an older protocol is refused at the handshake.
#[test]
fn a_proposal_under_another_evolution_rule_is_refused_before_derivation() {
    let vocabulary = train("test:protocol:rule", &CYCLE);
    let mut peer = ready_peer(Policy { accept_proposals: true, ..open() });
    let digest = peer.add_vocabulary(vocabulary).unwrap();
    let additions = vec![vec!["VALVE_7_STUCK".to_string()]];

    // What a v0.5 peer would send: the earlier rule, or no rule field at all.
    for rule in ["kute 0.5.0", "kute evolve-1", ""] {
        let propose = Message::SymbolPropose { digest: digest.clone(), additions: additions.clone(), version: "0.2.0".into(), rule: rule.into() };
        let reply = peer.handle(propose);
        assert_eq!(code(&reply), ErrorCode::UnsupportedRule, "rule {rule:?}");
        assert_eq!(peer.digests(), vec![digest.clone()], "nothing was derived or held for rule {rule:?}");
    }
    let without_rule = format!(r#"{{"type":"SYMBOL_PROPOSE","digest":"{digest}","additions":[["VALVE_7_STUCK"]],"version":"0.2.0"}}"#);
    let old: Message = serde_json::from_str(&without_rule).unwrap();
    assert_eq!(code(&peer.handle(old)), ErrorCode::UnsupportedRule);

    // The same proposal under this build's rule is taken.
    let propose = Message::SymbolPropose { digest: digest.clone(), additions, version: "0.2.0".into(), rule: EVOLUTION_RULE.into() };
    assert!(matches!(peer.handle(propose)[0], Message::VocabAccept { .. }));
    assert_eq!(peer.digests().len(), 2);

    // Capabilities name the rule, so an initiator can decline to propose.
    match peer.capabilities() {
        Message::Capabilities { evolution_rules, kvf_versions, .. } => {
            assert_eq!(evolution_rules, vec![EVOLUTION_RULE.to_string()]);
            assert!(!kvf_versions.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // A peer speaking the previous protocol does not get past HELLO.
    let mut fresh = Peer::new("b", Engine::new(), open());
    assert_eq!(code(&fresh.handle(Message::Hello { protocol: 1, peer: "old".into(), software: "kute 0.6.0".into() })), ErrorCode::UnsupportedProtocol);
}

/// The initiator's side of the same gate: it does not send a proposal to a
/// peer whose capabilities name another rule.
#[test]
fn an_initiator_does_not_propose_to_a_peer_with_another_rule() {
    struct OtherRule {
        peer: Peer,
        queue: std::collections::VecDeque<Message>,
        proposals_seen: std::rc::Rc<std::cell::Cell<u32>>,
    }
    impl Transport for OtherRule {
        fn send(&mut self, message: &Message) -> kute::Result<()> {
            if matches!(message, Message::SymbolPropose { .. }) {
                self.proposals_seen.set(self.proposals_seen.get() + 1);
            }
            for mut reply in self.peer.handle(message.clone()) {
                if let Message::Capabilities { evolution_rules, .. } = &mut reply {
                    *evolution_rules = vec!["kute evolve-1".to_string()];
                }
                self.queue.push_back(reply);
            }
            Ok(())
        }
        fn recv(&mut self) -> kute::Result<Option<Message>> {
            Ok(self.queue.pop_front())
        }
    }
    let vocabulary = train("test:protocol:rule-initiator", &CYCLE);
    let seen = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut far = Peer::new("far", Engine::new(), Policy { accept_proposals: true, ..open() });
    far.add_vocabulary(vocabulary.clone()).unwrap();
    let mut near = Peer::new("near", Engine::new(), open());
    let digest = near.add_vocabulary(vocabulary).unwrap();
    let mut session = Session::new(near, OtherRule { peer: far, queue: Default::default(), proposals_seen: seen.clone() });
    let remote = session.connect().unwrap();
    assert_eq!(remote.evolution_rules, vec!["kute evolve-1".to_string()]);
    let err = session.propose(&digest, &[vec!["X".to_string()]], "0.2.0").unwrap_err();
    assert!(matches!(err, KuteError::Unsupported(_)), "{err}");
    assert_eq!(seen.get(), 0, "no proposal left this side");
    assert_eq!(session.peer.digests().len(), 1, "nothing was derived locally");
}

/// AC-702: what the other side can make a peer hold is bounded in all, not
/// only per vocabulary, and repeating a transfer does not add to it.
#[test]
fn the_other_side_cannot_pass_the_aggregate_budgets() {
    let vocabularies: Vec<Vocabulary> = (0..6).map(|i| train(&format!("test:protocol:budget-{i}"), &CYCLE)).collect();
    let tokens = vocabularies[0].len() as u64;

    // By count.
    let mut peer = ready_peer(Policy { max_held_vocabularies: 3, ..open() });
    let mut taken = 0;
    for round in 0..50 {
        for v in &vocabularies {
            match &give(&mut peer, v)[0] {
                Message::VocabAccept { .. } => taken += 1,
                Message::Error { code, .. } => assert_eq!(*code, ErrorCode::TooLarge, "round {round}"),
                other => panic!("{other:?}"),
            }
            assert!(peer.digests().len() <= 3);
        }
    }
    assert_eq!(peer.digests().len(), 3);
    // The three held ones were accepted every round without growing anything.
    assert_eq!(taken, 3 * 50);

    // By tokens in all: two fit, the third does not.
    let mut peer = ready_peer(Policy { max_held_tokens: 2 * tokens + 1, ..open() });
    assert!(matches!(give(&mut peer, &vocabularies[0])[0], Message::VocabAccept { .. }));
    assert!(matches!(give(&mut peer, &vocabularies[1])[0], Message::VocabAccept { .. }));
    assert_eq!(code(&give(&mut peer, &vocabularies[2])), ErrorCode::TooLarge);

    // The announcement may lie about the size; the data is counted again.
    let mut peer = ready_peer(Policy { max_held_tokens: tokens - 1, ..open() });
    let mut lie = VocabRef::of(&vocabularies[0]).unwrap();
    lie.tokens = 1;
    assert!(matches!(peer.handle(Message::VocabAnnounce { vocabulary: lie.clone() })[0], Message::VocabRequest { .. }));
    let data = Message::VocabData { digest: lie.digest, kvf: kute::vocab::to_vec(&vocabularies[0]).unwrap() };
    assert!(matches!(peer.handle(data)[0], Message::Error { .. }));
    assert!(peer.digests().is_empty());

    // Proposals count against the same budgets, before derivation.
    let mut peer = ready_peer(Policy { accept_proposals: true, max_held_vocabularies: 2, ..open() });
    let mut digest = peer.add_vocabulary(vocabularies[0].clone()).unwrap();
    for step in 0..20 {
        let propose = Message::SymbolPropose {
            digest: digest.clone(),
            additions: vec![vec![format!("NEW_{step}")]],
            version: format!("0.{}.0", step + 2),
            rule: EVOLUTION_RULE.into(),
        };
        match &peer.handle(propose)[0] {
            Message::VocabAccept { digest: next } => digest = next.clone(),
            Message::Error { code, .. } => assert_eq!(*code, ErrorCode::TooLarge),
            other => panic!("{other:?}"),
        }
        assert!(peer.digests().len() <= 2, "step {step}");
    }
    assert_eq!(peer.digests().len(), 2);
}

/// AC-702 and B5: the inbox is bounded, a full inbox answers BUSY and
/// delivers nothing, and a repeated message is not delivered twice.
#[test]
fn delivery_is_bounded_and_repeats_are_not_delivered_twice() {
    let vocabulary = train("test:protocol:delivery", &CYCLE);
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    let digest = sender.add_vocabulary(vocabulary.clone()).unwrap();
    let raw = events(&CYCLE);
    let message = |id: u64| sender.encode_message(id, &digest, &raw, UnknownPolicy::Strict).unwrap();

    let mut peer = ready_peer(Policy { max_inbox_messages: 5, replay_window: 8, ..open() });
    peer.add_vocabulary(vocabulary.clone()).unwrap();

    // Sustained sending without the application reading: never more than five held.
    let mut busy = 0;
    for id in 1..=2000u64 {
        match &peer.handle(message(id).0)[0] {
            Message::Ack { .. } => {}
            Message::Error { code, .. } => {
                assert_eq!(*code, ErrorCode::Busy);
                busy += 1;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(busy, 1995);
    let inbox = peer.take_inbox();
    assert_eq!(inbox.iter().map(|d| d.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);

    // After the application reads, delivery resumes; a message refused as
    // busy was not recorded, so sending it again delivers it once.
    let (again, expected) = message(6);
    let receipt = peer.handle(again.clone());
    assert_eq!(receipt[0], Message::Ack { id: 6, units: 4, sha256: expected });
    // The same message again: the same receipt, no second delivery.
    for _ in 0..10 {
        assert_eq!(peer.handle(again.clone()), receipt);
    }
    assert_eq!(peer.take_inbox().len(), 1);
    // Even with the inbox full, a repeat of a delivered message is answered.
    for id in 7..=11 {
        assert!(matches!(peer.handle(message(id).0)[0], Message::Ack { .. }));
    }
    assert_eq!(code(&peer.handle(message(12).0)), ErrorCode::Busy);
    assert_eq!(peer.handle(again.clone()), receipt);
    // The same id on other content is refused.
    let other = sender.encode_message(6, &digest, &events(&["MOTOR_START"]), UnknownPolicy::Strict).unwrap().0;
    assert_eq!(code(&peer.handle(other)), ErrorCode::Invalid);
    assert_eq!(peer.take_inbox().len(), 5);

    // The window is bounded too: after eight newer ids, id 6 is forgotten
    // and a repeat is delivered as new. That is the documented limit.
    for id in 13..=20 {
        assert!(matches!(peer.handle(message(id).0)[0], Message::Ack { .. }));
        peer.take_inbox();
    }
    assert_eq!(peer.handle(again)[0], receipt[0]);
    assert_eq!(peer.take_inbox().len(), 1, "outside the window a repeat is a new delivery");

    // Bytes are bounded as well as messages.
    let Message::EncodedMessage { container, .. } = message(1).0 else { unreachable!() };
    let kept = raw.len() + 4 * kute::TokenSequence::from_bytes(&container).unwrap().ids.len() + container.len();
    let mut small = ready_peer(Policy { max_inbox_bytes: kept as u64 * 2, ..open() });
    small.add_vocabulary(vocabulary).unwrap();
    assert!(matches!(small.handle(message(1).0)[0], Message::Ack { .. }));
    assert!(matches!(small.handle(message(2).0)[0], Message::Ack { .. }));
    assert_eq!(code(&small.handle(message(3).0)), ErrorCode::Busy);
}

/// A compressed container can hold thousands of times its size in ids. A
/// peer must refuse one that declares more tokens than its unit limit on
/// the header alone, before it decompresses anything: here two kilobytes
/// would otherwise cost eight megabytes, and the same shape scales to
/// gigabytes inside one frame.
#[test]
fn a_container_is_refused_on_its_declared_size_before_it_is_inflated() {
    let vocabulary = train("test:protocol:inflate", &CYCLE);
    let mut receiver = ready_peer(Policy { max_decoded_units: 1000, translate: true, ..Policy::default() });
    let digest = receiver.add_vocabulary(vocabulary.clone()).unwrap();
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    sender.add_vocabulary(vocabulary).unwrap();
    let (Message::EncodedMessage { container, .. }, _) = sender.encode_message(1, &digest, &events(&CYCLE), UnknownPolicy::Strict).unwrap() else {
        panic!("expected an encoded message");
    };
    let mut sequence = kute::TokenSequence::from_bytes(&container).unwrap();
    sequence.ids = vec![sequence.ids[0]; 2_000_000];
    let bomb = sequence.to_bytes_compressed().unwrap();
    assert!(bomb.len() < 4096, "{} bytes", bomb.len());

    // The reader itself: refused by the count in the header.
    let refused = kute::TokenSequence::from_bytes_bounded(&bomb, 1000).unwrap_err();
    assert!(matches!(refused, KuteError::LimitExceeded(_)), "{refused}");
    assert!(refused.to_string().contains("declares 2000000 tokens"), "{refused}");
    assert_eq!(kute::TokenSequence::from_bytes_bounded(&bomb, 2_000_000).unwrap().ids.len(), 2_000_000);

    // The peer, for a message and for a translation request.
    for request in [Message::EncodedMessage { id: 9, container: bomb.clone() }, Message::Translate { id: 9, container: bomb.clone(), to_digest: digest.clone() }] {
        match &receiver.handle(request)[0] {
            Message::Error { code: ErrorCode::TooLarge, message } => assert!(message.contains("declares 2000000 tokens"), "{message}"),
            other => panic!("expected TOO_LARGE on the declared size, got {other:?}"),
        }
    }
    assert!(receiver.take_inbox().is_empty());
}

/// Review finding: the default policy took nothing but gave everything. A
/// peer now gives out files, answers and translations only when told to,
/// and only for the vocabularies the other side may use.
#[test]
fn a_peer_gives_only_what_its_policy_says() {
    let ours = train("test:protocol:ours", &CYCLE);
    let private = train("test:protocol:private", &["A", "B", "A", "C"]);
    let symbols = vec!["MOTOR_START".to_string()];
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    let d_ours = sender.add_vocabulary(ours.clone()).unwrap();
    let d_private = sender.add_vocabulary(private.clone()).unwrap();
    let container = |id: u64, digest: &str, names: &[&str]| match sender.encode_message(id, digest, &events(names), UnknownPolicy::Strict).unwrap().0 {
        Message::EncodedMessage { container, .. } => container,
        _ => unreachable!(),
    };

    // The default: messages are delivered, nothing else is answered.
    let mut peer = ready_peer(Policy::default());
    peer.add_vocabulary(ours.clone()).unwrap();
    peer.add_vocabulary(private.clone()).unwrap();
    assert_eq!(code(&peer.handle(Message::VocabRequest { digest: d_ours.clone() })), ErrorCode::Refused);
    assert_eq!(code(&peer.handle(Message::SymbolQuery { digest: d_ours.clone(), symbols: symbols.clone() })), ErrorCode::Refused);
    assert_eq!(code(&peer.handle(Message::Translate { id: 1, container: container(1, &d_ours, &CYCLE), to_digest: d_private.clone() })), ErrorCode::Refused);
    assert!(matches!(peer.handle(Message::EncodedMessage { id: 2, container: container(2, &d_ours, &CYCLE) })[0], Message::Ack { .. }));

    // Everything switched on, but for one vocabulary only: the other one
    // is answered exactly as a vocabulary that is not held.
    let listed = Policy { give_vocabularies: true, answer_queries: true, translate: true, vocabularies: Some([d_ours.clone()].into()), ..Policy::default() };
    let mut peer = ready_peer(listed.clone());
    peer.add_vocabulary(ours.clone()).unwrap();
    let mut without = peer.handle(Message::VocabRequest { digest: d_private.clone() });
    peer.add_vocabulary(private.clone()).unwrap();
    assert!(matches!(peer.handle(Message::VocabRequest { digest: d_ours.clone() })[0], Message::VocabData { .. }));
    assert!(matches!(peer.handle(Message::SymbolQuery { digest: d_ours.clone(), symbols: symbols.clone() })[0], Message::SymbolInfo { token_id: Some(_), .. }));
    let mut with = peer.handle(Message::VocabRequest { digest: d_private.clone() });
    assert_eq!(code(&with), ErrorCode::UnknownVocabulary);
    assert_eq!(with.pop(), without.pop(), "held-but-hidden and not-held must be indistinguishable");
    assert_eq!(code(&peer.handle(Message::SymbolQuery { digest: d_private.clone(), symbols: vec!["A".into()] })), ErrorCode::UnknownVocabulary);
    assert_eq!(code(&peer.handle(Message::EncodedMessage { id: 3, container: container(3, &d_private, &["A", "B"]) })), ErrorCode::UnknownVocabulary);
    assert_eq!(code(&peer.handle(Message::Translate { id: 4, container: container(4, &d_ours, &CYCLE), to_digest: d_private.clone() })), ErrorCode::UnknownVocabulary);
    assert_eq!(code(&peer.handle(Message::Translate { id: 5, container: container(5, &d_private, &["A"]), to_digest: d_ours.clone() })), ErrorCode::UnknownVocabulary);
    // Announcing the hidden one is answered exactly as announcing one
    // that is not held, whether or not the peer may add vocabularies.
    let announce = || Message::VocabAnnounce { vocabulary: kute_protocol::VocabRef::of(&private).unwrap() };
    for accept_vocabularies in [false, true] {
        let policy = Policy { accept_vocabularies, vocabularies: Some([d_ours.clone()].into()), ..Policy::default() };
        let mut hides = ready_peer(policy.clone());
        hides.add_vocabulary(ours.clone()).unwrap();
        hides.add_vocabulary(private.clone()).unwrap();
        let mut lacks = ready_peer(policy);
        lacks.add_vocabulary(ours.clone()).unwrap();
        let reply = hides.handle(announce());
        assert_eq!(reply, lacks.handle(announce()), "accepting {accept_vocabularies}");
        assert_eq!(matches!(reply[0], Message::VocabRequest { .. }), accept_vocabularies);
    }
    assert!(peer.take_inbox().is_empty());

    // A vocabulary the other side adds itself is one it may then use.
    let mut peer = ready_peer(Policy { accept_vocabularies: true, ..listed });
    peer.add_vocabulary(ours.clone()).unwrap();
    assert!(matches!(give(&mut peer, &private)[0], Message::VocabAccept { .. }));
    assert!(matches!(peer.handle(Message::EncodedMessage { id: 6, container: container(6, &d_private, &["A", "B"]) })[0], Message::Ack { .. }));
}

/// Review findings: a proposal whose version is already held with other
/// content is a conflict and is reported as one, a repeat of the same
/// proposal is accepted again, and the inbox budget counts what is kept.
#[test]
fn a_conflicting_proposal_is_a_conflict_and_the_inbox_counts_what_it_keeps() {
    let vocabulary = train("test:protocol:conflict", &CYCLE);
    let mut peer = ready_peer(Policy { accept_proposals: true, ..open() });
    let digest = peer.add_vocabulary(vocabulary.clone()).unwrap();
    let propose = |symbol: &str| Message::SymbolPropose { digest: digest.clone(), additions: vec![vec![symbol.to_string()]], version: "0.2.0".into(), rule: EVOLUTION_RULE.into() };
    let first = peer.handle(propose("VALVE_STUCK"));
    assert!(matches!(first[0], Message::VocabAccept { .. }));
    assert_eq!(peer.handle(propose("VALVE_STUCK")), first, "a repeat is accepted again");
    assert_eq!(code(&peer.handle(propose("PUMP_DRY"))), ErrorCode::VocabularyConflict);
    assert_eq!(peer.digests().len(), 2);

    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    let digest = sender.add_vocabulary(vocabulary.clone()).unwrap();
    let raw = events(&CYCLE);
    let (message, _) = sender.encode_message(1, &digest, &raw, UnknownPolicy::Strict).unwrap();
    // Room for the decoded bytes alone is not room for the message.
    let mut small = ready_peer(Policy { max_inbox_bytes: raw.len() as u64, ..Policy::default() });
    small.add_vocabulary(vocabulary).unwrap();
    assert_eq!(code(&small.handle(message)), ErrorCode::Busy);
}

/// A symbol may be a kilobyte long, and an escaped symbol is chosen by the
/// sender. A message of a few hundred bytes that repeats one long escaped
/// symbol stays under a limit counted in symbols and still decodes to
/// megabytes, so the bytes of the names are bounded as well: by the inbox
/// budget for a message and by the frame for a translation.
#[test]
fn long_symbols_are_bounded_by_their_bytes_not_only_their_count() {
    let vocabulary = train("test:protocol:long-symbols", &CYCLE);
    let limit = 100_000;
    let policy = Policy { max_decoded_units: limit, max_inbox_bytes: 1 << 20, max_frame_bytes: 1 << 20, translate: true, ..Policy::default() };
    let mut receiver = ready_peer(policy);
    let digest = receiver.add_vocabulary(vocabulary.clone()).unwrap();
    let mut sender = Peer::new("a", Engine::new(), Policy::default());
    sender.add_vocabulary(vocabulary.clone()).unwrap();
    let long = events(&["X".repeat(1024).as_str()]);
    let (Message::EncodedMessage { container, .. }, _) = sender.encode_message(1, &digest, &long, UnknownPolicy::Escape).unwrap() else {
        panic!("expected an encoded message");
    };
    let mut sequence = kute::TokenSequence::from_bytes(&container).unwrap();
    assert_eq!(sequence.escapes.len(), 1);
    // 10,000 symbols, a tenth of the limit; ten megabytes of names.
    sequence.ids = vec![sequence.ids[0]; 10_000];
    assert_eq!(kute::decoded_units(&vocabulary, &sequence).unwrap(), 10_000);
    assert_eq!(kute::decoded_bytes_at_least(&vocabulary, &sequence).unwrap(), 10_240_000);
    let small = sequence.to_bytes_compressed().unwrap();
    assert!(small.len() < 4096, "{} bytes", small.len());

    let engine = Engine::new();
    let tokenizer = engine.tokenizer(vocabulary).unwrap();
    let refused = engine.decode_raw_within(tokenizer.as_ref(), &sequence, limit, 1 << 20).unwrap_err();
    assert!(matches!(refused, KuteError::LimitExceeded(_)), "{refused}");
    for request in [Message::EncodedMessage { id: 5, container: small.clone() }, Message::Translate { id: 5, container: small.clone(), to_digest: digest.clone() }] {
        match &receiver.handle(request)[0] {
            Message::Error { code: ErrorCode::TooLarge, message } => assert!(message.contains("at least 10240000 bytes"), "{message}"),
            other => panic!("expected TOO_LARGE on the bytes, got {other:?}"),
        }
    }
    assert!(receiver.take_inbox().is_empty());
    // The same symbol sent once is delivered.
    assert!(matches!(receiver.handle(Message::EncodedMessage { id: 6, container })[0], Message::Ack { .. }));
}
