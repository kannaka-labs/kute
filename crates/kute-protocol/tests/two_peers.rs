//! Gate from the development directive v0.2: "Two independently configured
//! peers negotiate a vocabulary, reject mismatches and exchange encoded
//! messages."

use kute::{AdapterConfig, Engine, KuteError, TrainingConfig, UnknownPolicy, Version, Vocabulary, VocabularyId};
use kute_protocol::{ErrorCode, Framed, Message, Peer, Policy, Session, Transport, VocabRef, DEFAULT_MAX_FRAME, PROTOCOL_VERSION};
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
    let (address, handle) = responder(Policy { accept_proposals: true, ..Policy::default() }, Vec::new());
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
    let mut peer = ready_peer(Policy::default());
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
    let mut small = ready_peer(Policy { max_vocabulary_tokens: 4, ..Policy::default() });
    assert_eq!(code(&small.handle(Message::VocabAnnounce { vocabulary: honest.clone() })), ErrorCode::TooLarge);

    // The honest transfer works, and then a message that would decode past
    // the budget is refused before decoding.
    let mut tight = ready_peer(Policy { max_decoded_units: 8, ..Policy::default() });
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
