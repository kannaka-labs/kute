//! Vocabulary exchange between two KUTE peers (development directive v0.2,
//! R9). Before one side sends encoded data, both must hold the same
//! vocabulary, identified by its content digest. The protocol lets them
//! find out, transfer the vocabulary if needed, extend it together, and
//! check that what was sent is what was decoded.
//!
//! [`Peer`] is the state machine: it takes a message and returns replies,
//! and does no I/O. [`Session`] drives the initiating side over a
//! [`Transport`]. [`Framed`] is a transport over any byte stream.
//!
//! There is no encryption and no authentication here. A peer treats
//! everything it receives as untrusted: vocabularies are validated and
//! their digests recomputed, sizes are bounded before allocation, and
//! decoding is bounded before output is produced.

use kute::{Engine, KuteError, Result, TokenPayload, TokenSequence, Tokenizer, UnknownPolicy, Version, Vocabulary};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u32 = 1;
/// Default and largest frame a peer will read.
pub const DEFAULT_MAX_FRAME: u64 = 64 * 1024 * 1024;

/// What identifies a vocabulary to the other side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabRef {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub algorithm: String,
    pub tokens: u64,
}

impl VocabRef {
    pub fn of(vocabulary: &Vocabulary) -> Result<Self> {
        Ok(Self {
            id: vocabulary.id().to_string(),
            version: vocabulary.version().to_string(),
            digest: vocabulary.digest()?,
            algorithm: vocabulary.manifest.algorithm.clone(),
            tokens: vocabulary.len() as u64,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A message arrived before the handshake allowed it.
    OutOfOrder,
    UnsupportedProtocol,
    UnsupportedAlgorithm,
    /// Same id and version as a vocabulary the peer holds, different content.
    VocabularyConflict,
    UnknownVocabulary,
    /// The vocabulary, message or proposal failed validation.
    Invalid,
    /// The peer's policy does not allow the request.
    Refused,
    TooLarge,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Message {
    Hello { protocol: u32, peer: String, software: String },
    Capabilities { algorithms: Vec<String>, adapters: Vec<String>, max_frame_bytes: u64, accepts_vocabularies: bool, accepts_proposals: bool },
    /// "I intend to use this vocabulary."
    VocabAnnounce { vocabulary: VocabRef },
    /// "I do not have it; send it."
    VocabRequest { digest: String },
    /// The vocabulary as a KVF document. Not in the directive's list; it is
    /// the answer to `VOCAB_REQUEST`.
    VocabData {
        digest: String,
        #[serde(with = "kute::b64")]
        kvf: Vec<u8>,
    },
    /// "I hold a validated vocabulary with this digest."
    VocabAccept { digest: String },
    /// "Is this symbol sequence a token of that vocabulary?"
    SymbolQuery { digest: String, symbols: Vec<String> },
    /// Answer to `SYMBOL_QUERY`.
    SymbolInfo { digest: String, token_id: Option<u32>, content_hash: String },
    /// "Extend that vocabulary with these symbols and sequences, as this
    /// version." Accepted with `VOCAB_ACCEPT` naming the new digest.
    SymbolPropose { digest: String, additions: Vec<Vec<String>>, version: String },
    /// "Re-encode this sequence under another vocabulary you hold."
    Translate {
        id: u64,
        #[serde(with = "kute::b64")]
        container: Vec<u8>,
        to_digest: String,
    },
    /// A `.kute` container.
    EncodedMessage {
        id: u64,
        #[serde(with = "kute::b64")]
        container: Vec<u8>,
    },
    /// Receipt for an encoded message: how much it decoded to, and the
    /// SHA-256 of the decoded bytes.
    Ack { id: u64, units: u64, sha256: String },
    Error { code: ErrorCode, message: String },
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> Vec<Message> {
    vec![Message::Error { code, message: message.into() }]
}

/// What a peer is willing to do for the other side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Take vocabularies it does not have from the other side.
    pub accept_vocabularies: bool,
    /// Extend a held vocabulary when the other side proposes it.
    pub accept_proposals: bool,
    pub max_frame_bytes: u64,
    pub max_vocabulary_tokens: u64,
    /// Refuse a message that would decode to more than this.
    pub max_decoded_units: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            accept_vocabularies: true,
            accept_proposals: false,
            max_frame_bytes: DEFAULT_MAX_FRAME,
            max_vocabulary_tokens: 1_000_000,
            max_decoded_units: 256 * 1024 * 1024,
        }
    }
}

/// A message this peer received and decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivered {
    pub id: u64,
    pub sequence: TokenSequence,
    /// The decoded bytes, through the vocabulary's adapter.
    pub bytes: Vec<u8>,
}

struct Held {
    vocabulary: Vocabulary,
    tokenizer: Box<dyn Tokenizer>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    New,
    Greeted,
    Ready,
}

/// One side of the protocol.
pub struct Peer {
    name: String,
    engine: Engine,
    policy: Policy,
    held: BTreeMap<String, Held>,
    stage: Stage,
    wanted: Option<VocabRef>,
    inbox: Vec<Delivered>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    kute::hex(&Sha256::digest(bytes))
}

impl Peer {
    pub fn new(name: &str, engine: Engine, policy: Policy) -> Self {
        Self { name: name.to_string(), engine, policy, held: BTreeMap::new(), stage: Stage::New, wanted: None, inbox: Vec::new() }
    }

    /// Hold `vocabulary`. Returns its digest.
    pub fn add_vocabulary(&mut self, vocabulary: Vocabulary) -> Result<String> {
        vocabulary.validate()?;
        let digest = vocabulary.compute_digest()?;
        if let Some(other) = self.held.values().find(|h| h.vocabulary.id() == vocabulary.id() && h.vocabulary.version() == vocabulary.version()) {
            if other.vocabulary.digest()? != digest {
                return Err(KuteError::VocabularyMismatch(format!(
                    "a different vocabulary is already held as {} {}",
                    vocabulary.id(),
                    vocabulary.version()
                )));
            }
        }
        let tokenizer = self.engine.tokenizer(vocabulary.clone())?;
        let vocabulary = tokenizer.vocabulary().cloned().unwrap_or(vocabulary);
        self.held.insert(digest.clone(), Held { vocabulary, tokenizer });
        Ok(digest)
    }

    pub fn vocabulary(&self, digest: &str) -> Option<&Vocabulary> {
        self.held.get(digest).map(|h| &h.vocabulary)
    }

    pub fn digests(&self) -> Vec<String> {
        self.held.keys().cloned().collect()
    }

    /// Messages received and decoded since the last call.
    pub fn take_inbox(&mut self) -> Vec<Delivered> {
        std::mem::take(&mut self.inbox)
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    fn hold(&self, digest: &str) -> Result<&Held> {
        self.held.get(digest).ok_or_else(|| KuteError::VocabularyMismatch(format!("no vocabulary with digest {digest} is held")))
    }

    pub fn hello(&self) -> Message {
        Message::Hello { protocol: PROTOCOL_VERSION, peer: self.name.clone(), software: format!("kute {}", kute::KUTE_VERSION) }
    }

    pub fn capabilities(&self) -> Message {
        Message::Capabilities {
            algorithms: self.engine.registry().algorithm_names(),
            adapters: self.engine.registry().adapter_names(),
            max_frame_bytes: self.policy.max_frame_bytes,
            accepts_vocabularies: self.policy.accept_vocabularies,
            accepts_proposals: self.policy.accept_proposals,
        }
    }

    /// Encode `raw` under a held vocabulary and wrap it as a message.
    /// Returns the message and the SHA-256 the receiver's `ACK` must carry.
    pub fn encode_message(&self, id: u64, digest: &str, raw: &[u8], policy: UnknownPolicy) -> Result<(Message, String)> {
        let held = self.hold(digest)?;
        let sequence = self.engine.encode_raw_open(held.tokenizer.as_ref(), raw, policy)?;
        let expected = sha256_hex(&self.engine.decode_raw(held.tokenizer.as_ref(), &sequence)?);
        Ok((Message::EncodedMessage { id, container: sequence.to_bytes_compressed()? }, expected))
    }

    /// Apply an evolution step to a held vocabulary and hold the result.
    pub fn evolve(&mut self, digest: &str, additions: &[Vec<String>], version: &str) -> Result<String> {
        let version = Version::parse(version).map_err(|e| KuteError::InvalidIdentifier(format!("version {version:?}: {e}")))?;
        let child = kute::bpe::sequence::evolve(&self.hold(digest)?.vocabulary, additions, version)?;
        self.add_vocabulary(child)
    }

    /// Re-encode a sequence under another held vocabulary. Symbols the
    /// target lacks are escaped, so nothing is lost.
    pub fn translate(&self, sequence: &TokenSequence, to_digest: &str) -> Result<TokenSequence> {
        let from = self.hold(&sequence.vocabulary_digest)?;
        let to = self.hold(to_digest)?;
        let units = kute::decoded_units(&from.vocabulary, sequence)?;
        if units > self.policy.max_decoded_units {
            return Err(KuteError::LimitExceeded(format!("the sequence decodes to {units} units")));
        }
        to.tokenizer.encode_open(&from.tokenizer.decode(sequence)?, UnknownPolicy::Escape)
    }

    /// Handle one message from the other side. Never fails: anything wrong
    /// is answered with an `ERROR` message.
    pub fn handle(&mut self, message: Message) -> Vec<Message> {
        match (self.stage, &message) {
            (Stage::New, Message::Hello { protocol, .. }) => {
                if *protocol != PROTOCOL_VERSION {
                    return refuse(ErrorCode::UnsupportedProtocol, format!("this peer speaks protocol {PROTOCOL_VERSION}, not {protocol}"));
                }
                self.stage = Stage::Greeted;
                return vec![self.hello()];
            }
            (Stage::Greeted, Message::Capabilities { .. }) => {
                self.stage = Stage::Ready;
                return vec![self.capabilities()];
            }
            (Stage::Ready, Message::Hello { .. } | Message::Capabilities { .. }) | (Stage::New | Stage::Greeted, _) => {
                return refuse(ErrorCode::OutOfOrder, "expected HELLO, then CAPABILITIES, before anything else");
            }
            (Stage::Ready, _) => {}
        }
        match self.respond(message) {
            Ok(replies) => replies,
            Err(e) => {
                let code = match e {
                    KuteError::LimitExceeded(_) => ErrorCode::TooLarge,
                    KuteError::VocabularyMismatch(_) => ErrorCode::UnknownVocabulary,
                    _ => ErrorCode::Invalid,
                };
                refuse(code, e.to_string())
            }
        }
    }

    fn respond(&mut self, message: Message) -> Result<Vec<Message>> {
        match message {
            Message::VocabAnnounce { vocabulary } => {
                if self.held.contains_key(&vocabulary.digest) {
                    return Ok(vec![Message::VocabAccept { digest: vocabulary.digest }]);
                }
                if self.held.values().any(|h| h.vocabulary.id().as_str() == vocabulary.id && h.vocabulary.version().to_string() == vocabulary.version) {
                    return Ok(refuse(
                        ErrorCode::VocabularyConflict,
                        format!("{} {} is held here with different content", vocabulary.id, vocabulary.version),
                    ));
                }
                if !self.engine.registry().algorithm_names().contains(&vocabulary.algorithm) {
                    return Ok(refuse(ErrorCode::UnsupportedAlgorithm, format!("algorithm {:?} is not available here", vocabulary.algorithm)));
                }
                if !self.policy.accept_vocabularies {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not take vocabularies from the other side"));
                }
                if vocabulary.tokens > self.policy.max_vocabulary_tokens {
                    return Ok(refuse(ErrorCode::TooLarge, format!("vocabularies above {} tokens are not taken", self.policy.max_vocabulary_tokens)));
                }
                let digest = vocabulary.digest.clone();
                self.wanted = Some(vocabulary);
                Ok(vec![Message::VocabRequest { digest }])
            }
            Message::VocabRequest { digest } => {
                let kvf = kute::vocab::to_vec(&self.hold(&digest)?.vocabulary)?;
                Ok(vec![Message::VocabData { digest, kvf }])
            }
            Message::VocabData { digest, kvf } => {
                let Some(wanted) = self.wanted.take().filter(|w| w.digest == digest) else {
                    return Ok(refuse(ErrorCode::OutOfOrder, "vocabulary data that was not requested"));
                };
                let vocabulary = kute::vocab::from_slice(&kvf)?;
                // The digest is recomputed from content; the one in the file
                // and the one announced are both only claims.
                if vocabulary.compute_digest()? != digest || VocabRef::of(&vocabulary)? != wanted {
                    return Ok(refuse(ErrorCode::Invalid, "the vocabulary sent is not the one announced"));
                }
                if vocabulary.len() as u64 > self.policy.max_vocabulary_tokens {
                    return Ok(refuse(ErrorCode::TooLarge, "vocabulary exceeds the token limit"));
                }
                let report = self.engine.verify(&vocabulary)?;
                if !report.ok() {
                    return Ok(refuse(ErrorCode::Invalid, format!("the vocabulary fails its own test vectors: {}", report.failures.join("; "))));
                }
                let digest = self.add_vocabulary(vocabulary)?;
                Ok(vec![Message::VocabAccept { digest }])
            }
            Message::SymbolQuery { digest, symbols } => {
                let payload = TokenPayload::Symbols(symbols);
                let token_id = self.hold(&digest)?.vocabulary.tokens.iter().find(|t| t.payload == payload).map(|t| t.id);
                Ok(vec![Message::SymbolInfo { digest, token_id, content_hash: payload.content_hash() }])
            }
            Message::SymbolPropose { digest, additions, version } => {
                if !self.policy.accept_proposals {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not accept vocabulary proposals"));
                }
                let digest = self.evolve(&digest, &additions, &version)?;
                Ok(vec![Message::VocabAccept { digest }])
            }
            Message::Translate { id, container, to_digest } => {
                let sequence = TokenSequence::from_bytes(&container)?;
                let translated = self.translate(&sequence, &to_digest)?;
                Ok(vec![Message::EncodedMessage { id, container: translated.to_bytes_compressed()? }])
            }
            Message::EncodedMessage { id, container } => {
                let sequence = TokenSequence::from_bytes(&container)?;
                let held = self.hold(&sequence.vocabulary_digest)?;
                let units = kute::decoded_units(&held.vocabulary, &sequence)?;
                let bytes = self.engine.decode_raw_bounded(held.tokenizer.as_ref(), &sequence, self.policy.max_decoded_units)?;
                let sha256 = sha256_hex(&bytes);
                self.inbox.push(Delivered { id, sequence, bytes });
                Ok(vec![Message::Ack { id, units, sha256 }])
            }
            Message::VocabAccept { .. } | Message::SymbolInfo { .. } | Message::Ack { .. } | Message::Error { .. } => {
                Ok(refuse(ErrorCode::OutOfOrder, "a reply arrived where a request was expected"))
            }
            Message::Hello { .. } | Message::Capabilities { .. } => Ok(refuse(ErrorCode::OutOfOrder, "the handshake is already complete")),
        }
    }
}

/// Something that carries messages to the other peer and back.
pub trait Transport {
    fn send(&mut self, message: &Message) -> Result<()>;
    /// The next message, or `None` when the other side has closed.
    fn recv(&mut self) -> Result<Option<Message>>;
}

/// Messages over a byte stream: a four-byte little-endian length, then that
/// many bytes of JSON.
pub struct Framed<S: Read + Write> {
    stream: S,
    max_frame: u64,
}

impl<S: Read + Write> Framed<S> {
    pub fn new(stream: S, max_frame: u64) -> Self {
        Self { stream, max_frame: max_frame.min(DEFAULT_MAX_FRAME) }
    }
}

impl<S: Read + Write> Transport for Framed<S> {
    fn send(&mut self, message: &Message) -> Result<()> {
        let body = serde_json::to_vec(message)?;
        if body.len() as u64 > self.max_frame {
            return Err(KuteError::LimitExceeded(format!("message of {} bytes exceeds the frame limit of {}", body.len(), self.max_frame)));
        }
        self.stream.write_all(&(body.len() as u32).to_le_bytes())?;
        self.stream.write_all(&body)?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Message>> {
        let mut length = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            match self.stream.read(&mut length[got..]) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(KuteError::InvalidInput("the stream ended inside a frame header".into())),
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        let length = u32::from_le_bytes(length) as u64;
        if length > self.max_frame {
            return Err(KuteError::LimitExceeded(format!("incoming frame of {length} bytes exceeds the limit of {}", self.max_frame)));
        }
        // Read through `take` so a forged length cannot make us allocate
        // more than actually arrives.
        let mut body = Vec::new();
        (&mut self.stream).take(length).read_to_end(&mut body)?;
        if body.len() as u64 != length {
            return Err(KuteError::InvalidInput("the stream ended inside a frame".into()));
        }
        serde_json::from_slice(&body).map(Some).map_err(|e| KuteError::InvalidInput(format!("not a protocol message: {e}")))
    }
}

/// The other side's answer to `CAPABILITIES`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub peer: String,
    pub software: String,
    pub algorithms: Vec<String>,
    pub accepts_vocabularies: bool,
    pub accepts_proposals: bool,
}

fn peer_error(code: ErrorCode, message: String) -> KuteError {
    KuteError::Backend(format!("the peer answered {code:?}: {message}"))
}

/// A peer together with its connection.
pub struct Session<T: Transport> {
    pub peer: Peer,
    transport: T,
    next_id: u64,
}

impl<T: Transport> Session<T> {
    pub fn new(peer: Peer, transport: T) -> Self {
        Self { peer, transport, next_id: 1 }
    }

    fn ask(&mut self, message: &Message) -> Result<Message> {
        self.transport.send(message)?;
        match self.transport.recv()? {
            Some(Message::Error { code, message }) => Err(peer_error(code, message)),
            Some(reply) => Ok(reply),
            None => Err(KuteError::Backend("the peer closed the connection".into())),
        }
    }

    fn unexpected<R>(reply: Message) -> Result<R> {
        Err(KuteError::Backend(format!("unexpected reply from the peer: {reply:?}")))
    }

    /// Initiator: handshake and capability exchange.
    pub fn connect(&mut self) -> Result<Remote> {
        let (peer, software) = match self.ask(&self.peer.hello())? {
            Message::Hello { protocol, peer, software } if protocol == PROTOCOL_VERSION => (peer, software),
            other => return Self::unexpected(other),
        };
        match self.ask(&self.peer.capabilities())? {
            Message::Capabilities { algorithms, accepts_vocabularies, accepts_proposals, .. } => {
                Ok(Remote { peer, software, algorithms, accepts_vocabularies, accepts_proposals })
            }
            other => Self::unexpected(other),
        }
    }

    /// Initiator: make sure the other side holds the vocabulary `digest`,
    /// sending it if asked. Returns true if it had to be transferred.
    pub fn share(&mut self, digest: &str) -> Result<bool> {
        let vocabulary = VocabRef::of(&self.peer.hold(digest)?.vocabulary)?;
        match self.ask(&Message::VocabAnnounce { vocabulary })? {
            Message::VocabAccept { digest: d } if d == digest => Ok(false),
            Message::VocabRequest { digest: d } if d == digest => {
                let kvf = kute::vocab::to_vec(&self.peer.hold(digest)?.vocabulary)?;
                match self.ask(&Message::VocabData { digest: digest.to_string(), kvf })? {
                    Message::VocabAccept { digest: d } if d == digest => Ok(true),
                    other => Self::unexpected(other),
                }
            }
            other => Self::unexpected(other),
        }
    }

    /// Initiator: encode `raw`, send it, and check the receipt. Returns the
    /// number of units the other side decoded. Fails if the other side
    /// decoded anything other than what this side's decoder gives.
    pub fn send(&mut self, digest: &str, raw: &[u8], policy: UnknownPolicy) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let (message, expected) = self.peer.encode_message(id, digest, raw, policy)?;
        match self.ask(&message)? {
            Message::Ack { id: acked, units, sha256 } if acked == id => {
                if sha256 != expected {
                    return Err(KuteError::Integrity("the peer decoded the message to different content".into()));
                }
                Ok(units)
            }
            other => Self::unexpected(other),
        }
    }

    /// Initiator: extend a shared vocabulary on both sides. Each side
    /// applies the step itself; the digests must agree. Returns the new
    /// digest. Nothing changes locally if the other side refuses.
    pub fn propose(&mut self, digest: &str, additions: &[Vec<String>], version: &str) -> Result<String> {
        let parsed = Version::parse(version).map_err(|e| KuteError::InvalidIdentifier(format!("version {version:?}: {e}")))?;
        let child = kute::bpe::sequence::evolve(&self.peer.hold(digest)?.vocabulary, additions, parsed)?;
        let expected = child.digest()?;
        let propose = Message::SymbolPropose { digest: digest.to_string(), additions: additions.to_vec(), version: version.to_string() };
        match self.ask(&propose)? {
            Message::VocabAccept { digest: theirs } if theirs == expected => self.peer.add_vocabulary(child),
            Message::VocabAccept { digest: theirs } => {
                Err(KuteError::Integrity(format!("the peer derived {theirs}, this side derived {expected}")))
            }
            other => Self::unexpected(other),
        }
    }

    /// Initiator: ask whether a symbol sequence is a token over there.
    pub fn query(&mut self, digest: &str, symbols: &[String]) -> Result<Option<u32>> {
        match self.ask(&Message::SymbolQuery { digest: digest.to_string(), symbols: symbols.to_vec() })? {
            Message::SymbolInfo { token_id, .. } => Ok(token_id),
            other => Self::unexpected(other),
        }
    }

    /// Initiator: have the other side re-encode a sequence under another
    /// vocabulary it holds.
    pub fn translate(&mut self, sequence: &TokenSequence, to_digest: &str) -> Result<TokenSequence> {
        let id = self.next_id;
        self.next_id += 1;
        let ask = Message::Translate { id, container: sequence.to_bytes_compressed()?, to_digest: to_digest.to_string() };
        match self.ask(&ask)? {
            Message::EncodedMessage { id: got, container } if got == id => {
                let translated = TokenSequence::from_bytes(&container)?;
                if translated.vocabulary_digest != to_digest {
                    return Err(KuteError::Integrity("the peer translated to a different vocabulary".into()));
                }
                Ok(translated)
            }
            other => Self::unexpected(other),
        }
    }

    /// Responder: answer messages until the other side closes. Returns the
    /// number of messages handled.
    pub fn serve(&mut self) -> Result<u64> {
        let mut handled = 0;
        while let Some(message) = self.transport.recv()? {
            for reply in self.peer.handle(message) {
                self.transport.send(&reply)?;
            }
            handled += 1;
        }
        Ok(handled)
    }
}
