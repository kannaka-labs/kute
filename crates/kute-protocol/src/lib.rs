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

#[cfg(feature = "tls")]
pub mod tls;

/// Version 2 added the evolution rule to `CAPABILITIES` and to
/// `SYMBOL_PROPOSE`, the `UNSUPPORTED_RULE` and `BUSY` errors, and the
/// delivery contract for repeated message ids. A version 1 peer is refused
/// at `HELLO`.
pub const PROTOCOL_VERSION: u32 = 2;
/// The rule by which this build derives an evolved vocabulary. Two peers
/// that name different rules would derive different children from the same
/// proposal, so a proposal is only taken under this exact name.
pub const EVOLUTION_RULE: &str = kute::bpe::sequence::EVOLVE_GENERATOR;
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
    /// The proposal names an evolution rule this peer does not implement.
    UnsupportedRule,
    /// The peer's inbox is full; nothing was delivered. Send again later.
    Busy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Message {
    Hello { protocol: u32, peer: String, software: String },
    Capabilities {
        algorithms: Vec<String>,
        adapters: Vec<String>,
        max_frame_bytes: u64,
        accepts_vocabularies: bool,
        accepts_proposals: bool,
        /// Evolution rules this peer can apply to a proposal.
        #[serde(default)]
        evolution_rules: Vec<String>,
        /// KVF versions this peer reads.
        #[serde(default)]
        kvf_versions: Vec<String>,
    },
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
    /// `rule` names the derivation the proposer used; a peer that does not
    /// implement exactly that rule refuses before deriving anything.
    SymbolPropose {
        digest: String,
        additions: Vec<Vec<String>>,
        version: String,
        #[serde(default)]
        rule: String,
    },
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
///
/// The default takes nothing new from the other side (no vocabularies, no
/// proposals) and gives nothing away (no vocabulary files, no answers
/// about symbols, no translation). It delivers messages encoded under a
/// vocabulary this peer holds, and that is all. Everything else has to be
/// switched on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Send a held vocabulary's file to the other side when it asks.
    pub give_vocabularies: bool,
    /// Say whether a held vocabulary has a token for given symbols.
    pub answer_queries: bool,
    /// Re-encode the other side's sequences under another held vocabulary.
    pub translate: bool,
    /// The held vocabularies the other side may use at all, by digest;
    /// `None` is every one held. One that is held but not listed is
    /// answered as if it were not held. A vocabulary the other side adds
    /// itself (when it may) joins the list.
    pub vocabularies: Option<std::collections::BTreeSet<String>>,
    /// Refuse a container that declares more tokens than this, before its
    /// ids are decompressed. Four bytes of memory per token.
    pub max_message_tokens: u64,
    /// Take vocabularies it does not have from the other side.
    pub accept_vocabularies: bool,
    /// Extend a held vocabulary when the other side proposes it.
    pub accept_proposals: bool,
    pub max_frame_bytes: u64,
    pub max_vocabulary_tokens: u64,
    /// Refuse a message that would decode to more than this.
    pub max_decoded_units: u64,
    /// The most vocabularies this peer will hold, counting its own. The
    /// other side cannot add one past this.
    pub max_held_vocabularies: u64,
    /// The most tokens over all held vocabularies, counting its own.
    pub max_held_tokens: u64,
    /// Delivered messages kept until `take_inbox`. When full, a message is
    /// answered `BUSY` and not delivered.
    pub max_inbox_messages: u64,
    /// Bytes kept until `take_inbox`: for each message its decoded bytes,
    /// its token ids at four bytes each, and its container.
    pub max_inbox_bytes: u64,
    /// How many of the latest message ids are remembered, so that a repeat
    /// gets its first receipt again instead of a second delivery.
    pub replay_window: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            give_vocabularies: false,
            answer_queries: false,
            translate: false,
            vocabularies: None,
            max_message_tokens: 16 * 1024 * 1024,
            accept_vocabularies: false,
            accept_proposals: false,
            max_frame_bytes: DEFAULT_MAX_FRAME,
            max_vocabulary_tokens: 1_000_000,
            max_decoded_units: 256 * 1024 * 1024,
            max_held_vocabularies: 64,
            max_held_tokens: 4_000_000,
            max_inbox_messages: 1024,
            max_inbox_bytes: 256 * 1024 * 1024,
            replay_window: 1024,
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
    inbox_bytes: u64,
    /// The latest message ids: id, SHA-256 of the container, receipt sent.
    seen: std::collections::VecDeque<(u64, String, Message)>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    kute::hex(&Sha256::digest(bytes))
}

impl Peer {
    pub fn new(name: &str, engine: Engine, policy: Policy) -> Self {
        Self {
            name: name.to_string(),
            engine,
            policy,
            held: BTreeMap::new(),
            stage: Stage::New,
            wanted: None,
            inbox: Vec::new(),
            inbox_bytes: 0,
            seen: Default::default(),
        }
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
        self.inbox_bytes = 0;
        std::mem::take(&mut self.inbox)
    }

    /// Whether the other side may add a vocabulary of `tokens` tokens: the
    /// refusal if it would pass what this peer is willing to hold in all.
    fn over_budget(&self, tokens: u64) -> Option<Vec<Message>> {
        if self.held.len() as u64 >= self.policy.max_held_vocabularies {
            return Some(refuse(ErrorCode::TooLarge, format!("this peer already holds {} vocabularies, its limit", self.held.len())));
        }
        let held: u64 = self.held.values().map(|h| h.vocabulary.len() as u64).sum();
        if held.saturating_add(tokens) > self.policy.max_held_tokens {
            return Some(refuse(
                ErrorCode::TooLarge,
                format!("this peer holds {held} tokens and takes at most {} in all", self.policy.max_held_tokens),
            ));
        }
        None
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    fn hold(&self, digest: &str) -> Result<&Held> {
        self.held.get(digest).ok_or_else(|| KuteError::VocabularyMismatch(format!("no vocabulary with digest {digest} is held")))
    }

    /// Whether the other side may use the vocabulary with this digest.
    fn sees(&self, digest: &str) -> bool {
        self.policy.vocabularies.as_ref().is_none_or(|listed| listed.contains(digest))
    }

    /// A held vocabulary, for the other side: one it may not use is
    /// answered exactly as one that is not held.
    fn usable(&self, digest: &str) -> Result<&Held> {
        if !self.sees(digest) {
            return Err(KuteError::VocabularyMismatch(format!("no vocabulary with digest {digest} is held")));
        }
        self.hold(digest)
    }

    /// A vocabulary the other side added is one it may use.
    fn show(&mut self, digest: &str) {
        if let Some(listed) = &mut self.policy.vocabularies {
            listed.insert(digest.to_string());
        }
    }

    /// The most tokens a container from the other side may declare. Every
    /// token decodes to at least one unit, so the unit limit bounds it too.
    fn token_limit(&self) -> u64 {
        self.policy.max_message_tokens.min(self.policy.max_decoded_units)
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
            evolution_rules: vec![EVOLUTION_RULE.to_string()],
            kvf_versions: vec![kute::KVF_VERSION.to_string()],
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
        // Symbols can be a kilobyte each, so their count is not their size.
        // The translation goes back in one frame, so a frame bounds it.
        let bytes = kute::decoded_bytes_at_least(&from.vocabulary, sequence)?;
        if bytes > self.policy.max_frame_bytes {
            return Err(KuteError::LimitExceeded(format!("the sequence decodes to at least {bytes} bytes; a frame holds {}", self.policy.max_frame_bytes)));
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
                // A vocabulary that is held but not this peer's to use is
                // treated from here on exactly as one that is not held: the
                // same checks, the same request for its content. If the
                // peer then sends it, it had the content already, and it
                // becomes one it may use.
                if self.held.contains_key(&vocabulary.digest) && self.sees(&vocabulary.digest) {
                    return Ok(vec![Message::VocabAccept { digest: vocabulary.digest }]);
                }
                if self.held.values().any(|h| {
                    h.vocabulary.id().as_str() == vocabulary.id && h.vocabulary.version().to_string() == vocabulary.version && h.vocabulary.integrity.as_ref().is_none_or(|i| i.digest != vocabulary.digest)
                }) {
                    // This does tell a peer that the id and version are
                    // taken, whether or not it may use the holder. Two
                    // vocabularies cannot share them, so the refusal cannot
                    // be avoided, only worded the same in both cases.
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
                if let Some(refusal) = self.over_budget(vocabulary.tokens) {
                    return Ok(refusal);
                }
                let digest = vocabulary.digest.clone();
                self.wanted = Some(vocabulary);
                Ok(vec![Message::VocabRequest { digest }])
            }
            Message::VocabRequest { digest } => {
                if !self.policy.give_vocabularies {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not give out vocabularies"));
                }
                let kvf = kute::vocab::to_vec(&self.usable(&digest)?.vocabulary)?;
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
                // Again, on what was sent and not on what was announced.
                if let Some(refusal) = self.over_budget(vocabulary.len() as u64) {
                    return Ok(refusal);
                }
                let report = self.engine.verify(&vocabulary)?;
                if !report.ok() {
                    return Ok(refuse(ErrorCode::Invalid, format!("the vocabulary fails its own test vectors: {}", report.failures.join("; "))));
                }
                let digest = self.add_vocabulary(vocabulary)?;
                self.show(&digest);
                Ok(vec![Message::VocabAccept { digest }])
            }
            Message::SymbolQuery { digest, symbols } => {
                if !self.policy.answer_queries {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not answer questions about its vocabularies"));
                }
                let payload = TokenPayload::Symbols(symbols);
                let token_id = self.usable(&digest)?.vocabulary.tokens.iter().find(|t| t.payload == payload).map(|t| t.id);
                Ok(vec![Message::SymbolInfo { digest, token_id, content_hash: payload.content_hash() }])
            }
            Message::SymbolPropose { digest, additions, version, rule } => {
                if !self.policy.accept_proposals {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not accept vocabulary proposals"));
                }
                // Before anything is derived: under another rule the child
                // would differ, and the two sides would part silently.
                if rule != EVOLUTION_RULE {
                    return Ok(refuse(
                        ErrorCode::UnsupportedRule,
                        format!("the proposal uses evolution rule {rule:?}; this peer implements {EVOLUTION_RULE:?}"),
                    ));
                }
                // An addition of n symbols adds at most n symbols and n merges.
                let most: u64 = additions.iter().map(|a| 2 * a.len() as u64).sum();
                let parent = self.usable(&digest)?.vocabulary.len() as u64;
                if parent.saturating_add(most) > self.policy.max_vocabulary_tokens {
                    return Ok(refuse(ErrorCode::TooLarge, "the proposal would pass the vocabulary token limit"));
                }
                if let Some(refusal) = self.over_budget(parent.saturating_add(most)) {
                    return Ok(refusal);
                }
                // The child is derived even when its id and version are
                // already held: a repeat of the same proposal has to be
                // accepted again, and only the derivation tells a repeat
                // from a conflict. Its cost is bounded by the checks above.
                let digest = match self.evolve(&digest, &additions, &version) {
                    Ok(digest) => digest,
                    Err(KuteError::VocabularyMismatch(why)) => return Ok(refuse(ErrorCode::VocabularyConflict, why)),
                    Err(e) => return Err(e),
                };
                self.show(&digest);
                Ok(vec![Message::VocabAccept { digest }])
            }
            Message::Translate { id, container, to_digest } => {
                if !self.policy.translate {
                    return Ok(refuse(ErrorCode::Refused, "this peer does not translate for the other side"));
                }
                // A container that declares more tokens than the limit is
                // refused before its ids are decompressed.
                let sequence = TokenSequence::from_bytes_bounded(&container, self.token_limit())?;
                self.usable(&sequence.vocabulary_digest)?;
                self.usable(&to_digest)?;
                let translated = self.translate(&sequence, &to_digest)?;
                Ok(vec![Message::EncodedMessage { id, container: translated.to_bytes_compressed()? }])
            }
            Message::EncodedMessage { id, container } => {
                // A repeat of a delivered message gets its receipt again and
                // is not delivered twice; the same id on other content is an
                // error. Ids older than the window are not remembered.
                let fingerprint = sha256_hex(&container);
                if let Some((_, first, receipt)) = self.seen.iter().find(|(seen, _, _)| *seen == id) {
                    return Ok(if *first == fingerprint {
                        vec![receipt.clone()]
                    } else {
                        refuse(ErrorCode::Invalid, format!("message id {id} was already used for different content"))
                    });
                }
                if self.inbox.len() as u64 >= self.policy.max_inbox_messages {
                    return Ok(refuse(ErrorCode::Busy, format!("the inbox holds {} messages, its limit", self.inbox.len())));
                }
                let sequence = TokenSequence::from_bytes_bounded(&container, self.token_limit())?;
                let held = self.usable(&sequence.vocabulary_digest)?;
                let units = kute::decoded_units(&held.vocabulary, &sequence)?;
                let bytes = self.engine.decode_raw_within(held.tokenizer.as_ref(), &sequence, self.policy.max_decoded_units, self.policy.max_inbox_bytes)?;
                // What the inbox keeps for this message: the decoded bytes
                // and the sequence (ids, and the escapes the container holds).
                let kept = (bytes.len() as u64).saturating_add(4 * sequence.ids.len() as u64).saturating_add(container.len() as u64);
                if self.inbox_bytes.saturating_add(kept) > self.policy.max_inbox_bytes {
                    return Ok(refuse(ErrorCode::Busy, format!("the inbox holds {} bytes and takes at most {}", self.inbox_bytes, self.policy.max_inbox_bytes)));
                }
                let receipt = Message::Ack { id, units, sha256: sha256_hex(&bytes) };
                self.inbox_bytes += kept;
                self.inbox.push(Delivered { id, sequence, bytes });
                self.seen.push_back((id, fingerprint, receipt.clone()));
                while self.seen.len() as u64 > self.policy.replay_window {
                    self.seen.pop_front();
                }
                Ok(vec![receipt])
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
    pub evolution_rules: Vec<String>,
}

fn peer_error(code: ErrorCode, message: String) -> KuteError {
    KuteError::Backend(format!("the peer answered {code:?}: {message}"))
}

/// A peer together with its connection.
pub struct Session<T: Transport> {
    pub peer: Peer,
    transport: T,
    next_id: u64,
    remote: Option<Remote>,
}

impl<T: Transport> Session<T> {
    pub fn new(peer: Peer, transport: T) -> Self {
        Self { peer, transport, next_id: 1, remote: None }
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
            Message::Capabilities { algorithms, accepts_vocabularies, accepts_proposals, evolution_rules, .. } => {
                let remote = Remote { peer, software, algorithms, accepts_vocabularies, accepts_proposals, evolution_rules };
                self.remote = Some(remote.clone());
                Ok(remote)
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
        if let Some(remote) = &self.remote {
            if !remote.evolution_rules.iter().any(|r| r == EVOLUTION_RULE) {
                return Err(KuteError::Unsupported(format!(
                    "the peer implements evolution rules {:?}, not {EVOLUTION_RULE:?}; a proposal would derive different vocabularies on the two sides",
                    remote.evolution_rules
                )));
            }
        }
        let parsed = Version::parse(version).map_err(|e| KuteError::InvalidIdentifier(format!("version {version:?}: {e}")))?;
        let child = kute::bpe::sequence::evolve(&self.peer.hold(digest)?.vocabulary, additions, parsed)?;
        let expected = child.digest()?;
        let propose = Message::SymbolPropose {
            digest: digest.to_string(),
            additions: additions.to_vec(),
            version: version.to_string(),
            rule: EVOLUTION_RULE.to_string(),
        };
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
