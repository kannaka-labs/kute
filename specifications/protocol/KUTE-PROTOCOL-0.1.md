# KUTE vocabulary exchange protocol 0.1

Status: experimental. Protocol number 1. The message set and framing may
change before 1.0.

Two peers that want to exchange token sequences must hold the same
vocabulary. This protocol lets them establish that, transfer a vocabulary
when one side lacks it, extend a shared vocabulary together, and confirm that
an encoded message decoded to the intended content.

It provides no encryption and no authentication. Everything received is
untrusted input.

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
| `CAPABILITIES` | `algorithms`, `adapters`, `max_frame_bytes`, `accepts_vocabularies`, `accepts_proposals` | what this peer can and will do |
| `VOCAB_ANNOUNCE` | `vocabulary`: `id`, `version`, `digest`, `algorithm`, `tokens` | "I intend to use this vocabulary" |
| `VOCAB_REQUEST` | `digest` | "I do not hold it; send it" |
| `VOCAB_DATA` | `digest`, `kvf` | the vocabulary as a KVF document |
| `VOCAB_ACCEPT` | `digest` | "I hold a validated vocabulary with this digest" |
| `SYMBOL_QUERY` | `digest`, `symbols` | "is this symbol sequence a token there?" |
| `SYMBOL_INFO` | `digest`, `token_id` (or null), `content_hash` | the answer |
| `SYMBOL_PROPOSE` | `digest`, `additions`, `version` | "extend that vocabulary" (section 4) |
| `TRANSLATE` | `id`, `container`, `to_digest` | "re-encode this sequence under another vocabulary you hold" |
| `ENCODED_MESSAGE` | `id`, `container` | a `.kute` container |
| `ACK` | `id`, `units`, `sha256` | receipt: decoded length and SHA-256 of the decoded bytes |
| `ERROR` | `code`, `message` | refusal; the connection stays usable |

Error codes: `out_of_order`, `unsupported_protocol`, `unsupported_algorithm`,
`vocabulary_conflict`, `unknown_vocabulary`, `invalid`, `refused`,
`too_large`.

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
  as a symbol token; then a sequence of two or more symbols is folded from
  the left into merge tokens, reusing any merge that already exists;
* the version is replaced, `statistics.evolved_tokens` is set, and
  `provenance.training` becomes `{evolved_from: {version, digest},
  additions, parent_training}`;
* expected ids of the embedded test vectors are recomputed.

A proposal that adds nothing, does not raise the version or uses a reserved
token name is refused with `invalid`. The parent remains valid: sequences
encoded under it still name its digest. `TRANSLATE` moves a sequence to
another vocabulary by decoding and re-encoding it, with symbols the target
lacks carried as escapes (token envelope 2.2).

## 5. Not covered

Authentication, encryption, more than two peers, concurrent requests,
retransmission, and negotiation of anything other than a whole vocabulary.
