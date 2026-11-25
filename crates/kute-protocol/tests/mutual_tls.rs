//! Gate AC-701 of the v0.7 specification: authenticated peer exchange, and
//! "unauthorized peers are refused". Every certificate here is made for the
//! test run and trusted by nothing else.
#![cfg(feature = "tls")]

use kute::{AdapterConfig, Engine, TrainingConfig, UnknownPolicy, Vocabulary, VocabularyId};
use kute_protocol::tls::{self, Access, PeerIdentity};
use kute_protocol::{Framed, Peer, Policy, Session, DEFAULT_MAX_FRAME};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::{TcpListener, TcpStream};

struct Authority {
    certificate: rcgen::Certificate,
    key: KeyPair,
}

struct Holder {
    chain: Vec<CertificateDer<'static>>,
    key: Vec<u8>,
}

impl Holder {
    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
    fn fingerprint(&self) -> String {
        tls::fingerprint(&self.chain[0])
    }
}

fn authority(name: &str) -> Authority {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
    let key = KeyPair::generate().unwrap();
    Authority { certificate: params.self_signed(&key).unwrap(), key }
}

impl Authority {
    fn issue(&self, name: &str) -> Holder {
        let mut params = CertificateParams::new(vec![name.to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.certificate, &self.key).unwrap();
        Holder { chain: vec![certificate.der().clone()], key: key.serialize_der() }
    }
    fn der(&self) -> Vec<CertificateDer<'static>> {
        vec![self.certificate.der().clone()]
    }
}

fn events(names: &[&str]) -> Vec<u8> {
    names.iter().map(|n| format!("{{\"event\":\"{n}\"}}\n")).collect::<String>().into_bytes()
}

const CYCLE: [&str; 4] = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];

fn vocabulary() -> Vocabulary {
    let mut log = Vec::new();
    for _ in 0..40 {
        log.extend(events(&CYCLE));
    }
    let config = TrainingConfig::new(VocabularyId::parse("test:tls:events").unwrap(), 24);
    Engine::new().train("sequence-bpe", "jsonl-events", &AdapterConfig::default(), &[log], &config).unwrap()
}

/// What happened to one connection at the listening side.
#[derive(Debug)]
enum Served {
    /// Refused before any protocol message was read.
    RefusedAtHandshake(String),
    /// Authenticated, then refused for not being listed.
    NotAuthorized(PeerIdentity),
    /// Served; the inbox afterwards and how many vocabularies were held.
    Done { who: PeerIdentity, delivered: Vec<Vec<u8>>, held: usize },
}

/// A listening peer on a real socket in its own thread, serving `connections`
/// connections one after another.
fn listen(server: &Holder, client_authority: &Authority, access: Access, connections: usize) -> (std::net::SocketAddr, std::thread::JoinHandle<Vec<Served>>) {
    let config = tls::server_config(server.chain.clone(), server.key(), client_authority.der()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut out = Vec::new();
        for _ in 0..connections {
            let (tcp, _) = listener.accept().unwrap();
            let (stream, who) = match tls::accept(&config, tcp) {
                Ok(pair) => pair,
                Err(e) => {
                    out.push(Served::RefusedAtHandshake(e.to_string()));
                    continue;
                }
            };
            let policy = match access.policy_for(&who) {
                Ok(policy) => policy,
                Err(_) => {
                    out.push(Served::NotAuthorized(who));
                    continue; // the stream is dropped: the connection closes
                }
            };
            let mut session = Session::new(Peer::new("plant-b", Engine::new(), policy), Framed::new(stream, DEFAULT_MAX_FRAME));
            // A sender that finishes says so; an unclean end would be an error.
            session.serve().unwrap();
            let held = session.peer.digests().len();
            out.push(Served::Done { who, delivered: session.peer.take_inbox().into_iter().map(|d| d.bytes).collect(), held });
        }
        out
    });
    (address, handle)
}

type ClientSession = Session<Framed<tls::ClientStream>>;

fn dial(address: std::net::SocketAddr, client: &Holder, server_authority: &Authority, held: &Vocabulary) -> kute::Result<(ClientSession, String, PeerIdentity)> {
    let config = tls::client_config(client.chain.clone(), client.key(), server_authority.der())?;
    let (stream, server) = tls::connect(&config, "localhost", TcpStream::connect(address).unwrap())?;
    let mut peer = Peer::new("plant-a", Engine::new(), Policy::default());
    let digest = peer.add_vocabulary(held.clone())?;
    Ok((Session::new(peer, Framed::new(stream, DEFAULT_MAX_FRAME)), digest, server))
}

#[test]
fn authenticated_peers_exchange_and_unauthorized_peers_are_refused() {
    let plant = authority("plant authority");
    let stranger_authority = authority("someone else's authority");
    let server = plant.issue("localhost");
    let trusted = plant.issue("plant-a"); // listed, may share vocabularies
    let reader = plant.issue("plant-c"); // listed, may add nothing
    let unlisted = plant.issue("plant-x"); // same authority, not listed
    let stranger = stranger_authority.issue("plant-a"); // claims the same name

    let mut access = Access::new();
    access.allow(&trusted.fingerprint(), Policy { accept_vocabularies: true, ..Policy::default() }).unwrap();
    access.allow(&reader.fingerprint(), Policy::default()).unwrap();
    assert!(access.allow("plant-a", Policy::default()).is_err(), "a name is not a fingerprint");
    assert_eq!(access.len(), 2);

    let vocabulary = vocabulary();
    let raw = events(&["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK", "MOTOR_START"]);
    let (address, handle) = listen(&server, &plant, access, 5);

    // 1. The listed peer: the vocabulary is transferred and a message
    //    arrives, with the receipt checked, over the encrypted connection.
    let (mut a, digest, seen_server) = dial(address, &trusted, &plant, &vocabulary).unwrap();
    assert_eq!(seen_server.fingerprint, server.fingerprint(), "the client learns which server it reached");
    a.connect().unwrap();
    assert!(a.share(&digest).unwrap());
    assert_eq!(a.send(&digest, &raw, UnknownPolicy::Strict).unwrap(), 5);
    drop(a);

    // 2. A listed peer with a narrower policy: authenticated, but the
    //    policy bound to its certificate lets it add nothing.
    let (mut c, digest_c, _) = dial(address, &reader, &plant, &vocabulary).unwrap();
    c.connect().unwrap();
    let refused = c.share(&digest_c).unwrap_err().to_string();
    assert!(refused.contains("Refused"), "{refused}");
    drop(c);

    // 3. A certificate from the trusted authority that is not listed: the
    //    handshake succeeds, authorization does not, nothing is served.
    //    (The close may already be seen while dialling.)
    if let Ok((mut x, _, _)) = dial(address, &unlisted, &plant, &vocabulary) {
        assert!(x.connect().is_err(), "an unlisted peer got an answer");
    }

    // 4. A certificate from another authority claiming the listed peer's
    //    name: refused in the handshake.
    if let Ok((mut s, _, _)) = dial(address, &stranger, &plant, &vocabulary) {
        assert!(s.connect().is_err(), "a stranger got an answer");
    }

    // 5. No TLS at all: a plain connection speaking the protocol directly.
    let mut plain = Session::new(Peer::new("plain", Engine::new(), Policy::default()), Framed::new(TcpStream::connect(address).unwrap(), DEFAULT_MAX_FRAME));
    assert!(plain.connect().is_err(), "an unencrypted peer got an answer");
    drop(plain);

    let served = handle.join().unwrap();
    match &served[0] {
        Served::Done { who, delivered, held } => {
            assert_eq!(who.fingerprint, trusted.fingerprint());
            assert_eq!(delivered, &vec![raw.clone()]);
            assert_eq!(*held, 1);
        }
        other => panic!("{other:?}"),
    }
    match &served[1] {
        Served::Done { who, delivered, held } => {
            assert_eq!(who.fingerprint, reader.fingerprint());
            assert!(delivered.is_empty());
            assert_eq!(*held, 0, "the narrower policy added nothing");
        }
        other => panic!("{other:?}"),
    }
    match &served[2] {
        Served::NotAuthorized(who) => assert_eq!(who.fingerprint, unlisted.fingerprint()),
        other => panic!("{other:?}"),
    }
    for refused in &served[3..5] {
        match refused {
            Served::RefusedAtHandshake(why) => assert!(why.starts_with("backend error: tls: handshake") || why.contains("tls: handshake"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
}

/// The client checks the server too: a server whose certificate does not
/// come from the authority the client trusts is not talked to.
#[test]
fn a_client_refuses_a_server_it_cannot_verify() {
    let plant = authority("plant authority");
    let other = authority("another authority");
    let impostor = other.issue("localhost");
    let client = plant.issue("plant-a");
    let mut access = Access::new();
    access.allow(&client.fingerprint(), Policy::default()).unwrap();
    let (address, handle) = listen(&impostor, &plant, access, 1);
    assert!(dial(address, &client, &plant, &vocabulary()).is_err());
    match &handle.join().unwrap()[0] {
        Served::RefusedAtHandshake(why) => assert!(why.contains("tls: handshake"), "{why}"),
        other => panic!("{other:?}"),
    }
}
