# KUTE vocabulary exchange protocol 0.1

Status: experimental. Protocol number 2. The message set and framing may
change before 1.0. Number 2 (KUTE v0.7) added the evolution rule to
`CAPABILITIES` and `SYMBOL_PROPOSE`, the errors `unsupported_rule` and
`busy`, the budgets of section 6 and the delivery contract of section 7. A
peer that says `protocol: 1` in `HELLO` is refused with
`unsupported_protocol`.

Two peers that want to exchange token sequences must hold the same
vocabulary. This protocol lets them establish that, transfer a vocabulary
when one side lacks it, extend a shared vocabulary together, and confirm that
an encoded message decoded to the intended content.

The messages carry no credentials, and everything received is untrusted
input. Encryption and authentication come from the transport the messages
run over (section 8); over a plain socket there is neither.

## 1. Framing

A frame is a four-byte little-endian length followed by that many bytes of
UTF-8 JSON, one message per frame. A reader rejects a length above its frame
limit (at most 64 MiB) before reading the body, and never allocates by the
declared length alone.

## 2. Messages

Every message is a JSON object with a `type`. Byte fields are standard
base64. A vocabulary is always named by its content digest (KVF section 4).

| type | fields | meaning |
|------|--------|---------|
| `HELLO` | `protocol`, `peer`, `software` | first message in each direction |
| `CAPABILITIES` | `algorithms`, `adapters`, `max_frame_bytes`, `accepts_vocabularies`, `accepts_proposals`, `evolution_rules`, `kvf_versions` | what this peer can and will do |
| `VOCAB_ANNOUNCE` | `vocabulary`: `id`, `version`, `digest`, `algorithm`, `tokens` | "I intend to use this vocabulary" |
| `VOCAB_REQUEST` | `digest` | "I do not hold it; send it" |
| `VOCAB_DATA` | `digest`, `kvf` | the vocabulary as a KVF document |
| `VOCAB_ACCEPT` | `digest` | "I hold a validated vocabulary with this digest" |
| `SYMBOL_QUERY` | `digest`, `symbols` | "is this symbol sequence a token there?" |
| `SYMBOL_INFO` | `digest`, `token_id` (or null), `content_hash` | the answer |
| `SYMBOL_PROPOSE` | `digest`, `additions`, `version`, `rule` | "extend that vocabulary by this rule" (section 4) |
| `TRANSLATE` | `id`, `container`, `to_digest` | "re-encode this sequence under another vocabulary you hold" |
| `ENCODED_MESSAGE` | `id`, `container` | a `.kute` container |
| `ACK` | `id`, `units`, `sha256` | receipt: decoded length and SHA-256 of the decoded bytes |
| `ERROR` | `code`, `message` | refusal; the connection stays usable |

Error codes: `out_of_order`, `unsupported_protocol`, `unsupported_algorithm`,
`vocabulary_conflict`, `unknown_vocabulary`, `invalid`, `refused`,
`too_large`, `unsupported_rule`, `busy`.

A peer takes nothing new from the other side unless its policy says so:
`accepts_vocabularies` and `accepts_proposals` are both false by default.

## 3. Exchange

The initiator sends one request at a time and reads one reply.

1. `HELLO` both ways. A peer answers a different `protocol` number with
   `unsupported_protocol`.
2. `CAPABILITIES` both ways. Any other message before this point is
   `out_of_order`.
3. `VOCAB_ANNOUNCE`. The responder answers:
   * `VOCAB_ACCEPT` if it holds that digest;
   * `ERROR vocabulary_conflict` if it holds the same id and version with a
     different digest. It must not replace its own;
   * `ERROR unsupported_algorithm`, `refused` or `too_large` by its own
     registry and policy;
   * otherwise `VOCAB_REQUEST`, answered by `VOCAB_DATA`.
4. On `VOCAB_DATA` the responder must: accept it only if it was requested;
   validate the document; recompute the digest from the content and compare
   it with the one announced; compare id, version, algorithm and token count
   with the announcement; run the embedded test vectors. Only then does it
   answer `VOCAB_ACCEPT`.
5. `ENCODED_MESSAGE`. The responder finds the vocabulary by the digest in
   the container header (`unknown_vocabulary` if it has none), checks the
   decoded size against its budget before decoding (`too_large`), decodes,
   and answers `ACK`. The sender compares `sha256` with the hash of its own
   decoding of the same sequence. A difference means the two sides do not
   agree on what the vocabulary means, and the message must be treated as
   not delivered.

## 4. Proposals

`SYMBOL_PROPOSE` names a vocabulary both sides hold, a list of additions
(each a list of one or more symbols) and a new version. A responder whose
policy allows it derives the child vocabulary and answers `VOCAB_ACCEPT` with
the digest of the child; the initiator derives the child the same way and
compares digests. No vocabulary data crosses the connection.

The derivation is deterministic and is defined for `sequence-bpe`
vocabularies:

* every token of the parent keeps its id and payload;
* for each addition in order, each symbol not yet in the alphabet is appended
  as a symbol token; then a sequence of two or more symbols is encoded with
  the merges present so far, and while more than one token remains, the
  first two are joined by a new merge token and the sequence is encoded
  again. Afterwards the addition on its own encodes as one token;
* the version is replaced, `generator` becomes `kute evolve-3` (the name of
  this rule, so that releases implementing it agree on the digest),
  `statistics.evolved_tokens` is set, and
  `provenance.training` becomes `{evolved_from: {version, digest},
  additions}`. The parent is named by digest and its own record is not
  copied, so a child's size does not depend on how many generations
  precede it;
* expected ids of the embedded test vectors are recomputed.

v0.6.0 implemented `kute evolve-2`, which derives the same tokens but
copied the parent's training record into the child (`parent_training`);
the digests differ, so it is a different rule.
Releases up to v0.5.0 implemented an earlier rule (a left fold over the
bare symbols, with `kute <release>` as generator). A peer on an earlier rule
derives a different child. Under protocol 1 that was found only afterwards,
when the digests differed. Under protocol 2 it is found first:
`CAPABILITIES` lists the rules a peer implements in `evolution_rules`, an
initiator does not propose to a peer that does not list its rule, and
`SYMBOL_PROPOSE` names its `rule`. A responder that does not implement
exactly that rule answers `unsupported_rule` before deriving anything; a
proposal without the field is treated the same way.

A proposal that adds nothing, does not raise the version or uses a reserved
token name is refused with `invalid`. The parent remains valid: sequences
encoded under it still name its digest. `TRANSLATE` moves a sequence to
another vocabulary by decoding and re-encoding it, with symbols the target
lacks carried as escapes (token envelope 2.2).

## 5. Not covered

More than two peers, concurrent requests, and negotiation of anything
other than a whole vocabulary. Over a plain socket a peer cannot tell who
it is talking to and its policy is one policy for everybody; use the
transport of section 8 on any network that is not trusted.

## 6. Budgets

What one peer can make another hold is bounded in all, not only per item.
Each limit is the receiving peer's own setting.

| limit | default | what happens past it |
|-------|---------|----------------------|
| frame size | 64 MiB | the frame is refused before it is read |
| tokens in one vocabulary | 1,000,000 | `too_large` |
| vocabularies held, its own included | 64 | `too_large`; nothing is added |
| tokens over all held vocabularies | 4,000,000 | `too_large`; nothing is added |
| units one message decodes to | 268,435,456 | `too_large`, before decoding |
| delivered messages not yet read by the application | 1,024 | `busy`; not delivered |
| decoded bytes not yet read by the application | 256 MiB | `busy`; not delivered |

The size in `VOCAB_ANNOUNCE` is a claim, so the budgets are applied again
to the vocabulary that actually arrives. A proposal is counted, before it
is derived, as the parent plus two tokens for every symbol it lists, which
is the most it can add. Announcing a vocabulary the peer already holds is
answered `VOCAB_ACCEPT` and adds nothing, however often it is repeated.

Not bounded by this protocol: the number of connections, the rate of
messages and CPU time. One `Peer` serves one connection, one message at a
time; whoever accepts connections has to bound those.

## 7. Delivery

`ENCODED_MESSAGE` carries an `id` chosen by the sender. The receiver's
contract:

* A message is delivered to the application at most once while its id is
  remembered, and the receipt `ACK` is sent only after it has been decoded
  and placed in the inbox.
* The receiver remembers the latest ids it delivered (1,024 by default)
  with the SHA-256 of each container. A message whose id and container
  match a remembered one is answered with the same `ACK` and is not
  delivered again, so a sender that lost a receipt can send again safely.
* A remembered id on a different container is answered `invalid`.
* A message answered `busy`, or with any other error, was not delivered and
  is not remembered; sending it again later is a first delivery.
* An id older than the window is not remembered, and a repeat of it is
  delivered as new. A sender that may repeat across more than the window
  has to detect that itself.

Nothing is retransmitted by the protocol; retrying is the sender's choice.
`TRANSLATE` has no effect on the receiver's state and may be repeated
freely.

## 7a. What a peer gives

A peer answers `VOCAB_REQUEST`, `SYMBOL_QUERY` and `TRANSLATE` only when
its policy says so; otherwise the answer is `REFUSED`. A policy may also
list the vocabularies the other side may use. A vocabulary that is held
but not listed is answered, in every message, exactly as one that is not
held (`UNKNOWN_VOCABULARY`); announcing it, or another vocabulary under
its id and version, is answered `REFUSED`. A vocabulary the other side
adds itself joins its list.

A container is refused (`TOO_LARGE`) on the token count in its header,
before its ids are decompressed.

## 8. Authenticated transport

The reference implementation runs the frames of section 1 over TLS 1.3
with a certificate on both sides (`kute_protocol::tls`, through `rustls`).

* The listening side requires a client certificate issued by an authority
  it was configured with, and the connecting side verifies the server's
  certificate and name the same way. Either failure ends the handshake;
  no protocol message is read.
* A peer's identity is the SHA-256 of the certificate it proved possession
  of, written `sha256:<hex>`. The `peer` field of `HELLO` is a label chosen
  by the sender and identifies nobody.
* The listening side keeps a list of identities, each with its own policy
  (what it may add, and the budgets of section 6). A peer whose certificate
  verifies but whose identity is not listed is disconnected before `HELLO`.
  The list is empty until identities are added to it.

* A side that is finished closes with TLS `close_notify`. A connection
  that ends without it is an error to the reader, not an end of session.

Not provided: revocation (remove the identity from the list), certificate
issuing or rotation, and any binding between an identity and particular
vocabularies. A listed peer may do whatever its policy allows.
