# Decision records

Decisions taken while building v0.1, numbered to match the list in the
product requirements where one exists. Each can be reopened with a new record.

## ADR-0001: Rust core, with bindings

The engine is a Rust workspace. Other languages get thin bindings over it
(Python in v0.1) and never reimplement tokenization, so there is one behavior
to test.

## ADR-0002: Workspace of small crates

Types, traits, algorithms, adapters, the file format, the engine, benchmarks
and the CLI are separate crates. A user who only needs to read KVF files
depends on `kute-vocab` and nothing else. The Python bindings sit outside the
workspace because they link an interpreter; `cargo test --workspace` needs no
Python.

## ADR-0003: Tokenizer trait

```rust
pub trait Tokenizer: Send + Sync {
    fn train(&mut self, corpus: &mut dyn CorpusSource, config: &TrainingConfig) -> Result<Vocabulary>;
    fn load(&mut self, vocabulary: Vocabulary) -> Result<()>;
    fn encode(&self, input: &CanonicalInput) -> Result<TokenSequence>;
    fn decode(&self, tokens: &TokenSequence) -> Result<CanonicalInput>;
    fn vocabulary(&self) -> Option<&Vocabulary>;
    fn capabilities(&self) -> TokenizerCapabilities;
}
```

Two differences from the sketch in the requirements:

* `vocabulary()` returns `Option`, because a tokenizer exists before it is
  trained or loaded. Returning a reference unconditionally would force a
  panic or a dummy vocabulary.
* `load` is added. Without it a stored vocabulary could not be used through
  the generic interface.

Algorithms and adapters are found by name through a `Registry`, which third
parties can extend.

## ADR-0004: Canonical input is bytes or symbols

Every adapter produces one of two things: a byte string, or a sequence of
named symbols. Byte algorithms and symbol algorithms each accept one. This is
the smallest model that covers text, binary, records and events; richer
structure (trees, numbers) is left for when an adapter needs it.

## ADR-0005: Native BPE is the default; rustbpe is an optional reference backend

The requirements name rustbpe as the first backend. Its only training entry
point takes a Python iterator, its type is a Python class, and its encoder
drops text its split pattern does not match. Making it the default would put
a Python interpreter under every build and make text encoding lossy.

So: `bpe` is a native implementation of the same algorithm (most frequent
pair, ties to the smallest pair, ids from 256), and `rustbpe` is available
as a separate crate, `backends/rustbpe`, through the same `Tokenizer` trait.
A test there checks that both learn identical merges and produce
identical encodings, and that vocabularies load in either. The rustbpe
backend verifies each encoding by decoding it and refuses input it would
alter.

Consequences: the backend needs Python at build and run time. Its rustbpe
dependency is pinned to a git revision, because the crates.io release of
rustbpe uses an older PyO3. crates.io does not accept git dependencies, so
from v0.2.0 the backend is its own unpublished crate, outside the workspace,
and the published crates carry no trace of it. Its `kute-ref` binary is the
`kute` command with the backend registered.

## ADR-0006: KVF 0.1 is one JSON document

Readable, diffable, and enough for vocabularies up to tens of thousands of
tokens (a 32K vocabulary is a few megabytes and loads in well under a
second). A binary encoding is deferred. The content digest currently depends
on the reference implementation's JSON serialization; a byte-level canonical
form is needed before 1.0 and is noted in the specification.

## ADR-0007: Identity is id, version and digest together

Token sequences carry the vocabulary's identifier, semantic version and
content digest, and decoding refuses a mismatch on any of the three. Published
versions are immutable: the writer will not replace a file with different
content unless forced.

## ADR-0008: Losslessness is declared per component and stored

Adapters and algorithms each state whether they are lossless, and both
statements are written into the vocabulary. Benchmarks check two things
separately: that decoding reproduces the adapter's canonical form, and that it
reproduces the raw bytes. A canonicalizing adapter passes the first and not
the second, by design.

## ADR-0009: Deterministic training

Same corpus and configuration, same digest. No timestamps by default, ordered
maps wherever order reaches output, a fixed tie-break in pair selection. The
test suite and the benchmark suite both check it.

## ADR-0010: Vocabularies carry their own test vectors

Training stores sample inputs with their expected ids in the vocabulary, and
verifies them before returning. Any implementation, in any language, can
check that it reads a given file the way the writer did.

## ADR-0011: tiktoken export is exact or refused

Export is defined only for byte BPE vocabularies with no duplicate token
contents. Anything else is an error. A Python test loads the export in
tiktoken and compares ids.

## ADR-0012: HRM support is a plugin behind a narrow trait

HRM work lives in `plugins/hrm` behind `RepresentationBackend`. Mappings are
versioned independently and referenced, not embedded, by vocabularies. The
core has no HRM dependency and makes no HRM claims. See
`docs/hrm-adapter-contract.md`.

## ADR-0013: The GPT-4 pre-tokenizer preset runs on an automaton engine

The preset pattern uses possessive quantifiers and one lookahead, which need
a backtracking regex engine. That engine was nearly all of the encode time.
For the preset only, KUTE compiles an equivalent pattern for the `regex`
crate and applies the lookahead by hand; any other pattern still runs as
written on `fancy-regex`. Texts of 1 MiB or more under the preset are cut
between a line feed and an ASCII letter, a position no branch of the preset
can match across, and the pieces are encoded on separate threads.

Both shortcuts must give exactly the ids of the plain path. Tests compare the
two engines chunk for chunk on several thousand inputs, and the parallel
encoder against a serial pass. The stored pattern in a vocabulary is
unchanged, so exports and other readers are unaffected.

## ADR-0014: A second container format with compressed ids

Format 1 stores varint ids and is simple to read. Format 2 stores fixed-width
ids inside a zlib stream. It exists because a token stream is still highly
repetitive, and leaving that on the table made KUTE files larger than gzip of
the original. Format 1 remains the default so that existing files and readers
keep working; `from_bytes` reads both. See section 2.1 of the token envelope
specification for the format and its reader limits.

## ADR-0015: License (provisional)

MIT or Apache-2.0 at the user's option, the common choice for Rust libraries.
This was chosen to make the repository complete and is the maintainers' to
confirm or change before first publication.

## Records from the development directive v0.2

The directive proposes ADR numbers 0016 to 0030 for its own topics. Numbers
are assigned here in the order decisions are actually taken, starting at
0016, so they will not match the directive's list one for one.

## ADR-0016: Unknown symbols are refused or escaped, never approximated

A frozen symbol vocabulary used to refuse any input holding a symbol outside
its alphabet. That is correct but makes a vocabulary unusable on a live
stream, where new names appear. Encoding now takes a policy. `strict` keeps
the old behavior and is the default. `escape` carries the unknown symbol
literally in the sequence's own escape table and gives it an id past the end
of the token table.

The vocabulary is not modified, so its digest and every earlier sequence
stay valid, and nothing is learned silently. Proposing escaped symbols as
vocabulary candidates is a separate, explicit step (vocabulary evolution).
An unknown-token placeholder was rejected because it loses the input.

## ADR-0017: Registry names are taken once

`register_algorithm` and `register_adapter` return an error when the name is
already registered; `replace_algorithm` and `replace_adapter` override on
purpose. A vocabulary names its algorithm, so a plugin that silently took
over the name `bpe` would change what existing files mean. This changes the
signature of the `register` functions, which now return `Result`.

## ADR-0018: Test vectors are optional, because they are excerpts

ADR-0010 stores samples of the training input in the vocabulary. For a
private corpus that publishes the data. `embed_test_vectors` (CLI:
`--no-test-vectors`) turns it off. The default stays on, since a vocabulary
that can check itself is worth more, and the choice is recorded in the
training provenance. Learned tokens are frequent fragments of the corpus and
remain visible in any vocabulary; that is inherent and is not hidden by this
switch.

## ADR-0019: Decoders can bound output before producing it

`decoded_units` computes the decoded length from the token table alone, and
`decode_raw_bounded` refuses a sequence over a caller's budget. Container
parsing was already bounded; this closes the remaining amplification, from
ids to output. The HRM plugin's `inner_product` now returns an error for
vectors of different dimensions instead of truncating to the shorter one.

## ADR-0020: Discovery is its own subsystem, not a tokenizer

`kute-discover` finds contiguous sequences and gapped pairs in symbol
records and depends on no algorithm crate. A tokenizer answers "how do I
encode this"; discovery answers "what recurs here", and its output is
evidence: support, the records a pattern occurs in, its first occurrences,
and cohesion, the factor by which it beats the frequency of its parts, in
bits. Patterns carry the same content hash a vocabulary token with that
payload has, so a discovered pattern can later be proposed as a token.

Two checks ship with it because a pattern miner always returns something:
`evaluate` re-measures every pattern on held-out data, and `shuffled` builds
a control with the same symbol counts and no order. Candidate counts are
capped so hostile input cannot exhaust memory. Timestamps and typed fields
are not used yet; a pattern is defined on order alone.

## ADR-0021: Vocabularies evolve by appending; versions are immutable

A new version of an event vocabulary is derived from its parent by a
deterministic step that appends tokens and never renumbers or changes an
existing one. The parent stays valid and keeps its digest. Because the step
is a pure function of parent, additions and version, two parties can each
apply it and compare digests instead of shipping the result.

Rejected: mutating a vocabulary in place when an unknown symbol appears
(nothing could then be decoded reliably later), and automatic adoption of
escaped or discovered symbols (a vocabulary would drift without anyone
deciding). Discovery and escaping produce candidates; a person or a policy
proposes them. Evolution is defined for `sequence-bpe` only for now.

## ADR-0022: A small request-reply protocol, with the peer logic free of I/O

`kute-protocol` implements the message set of the directive for two peers
over length-prefixed JSON. `Peer::handle` maps one message to replies and
touches no socket, so it is tested directly with hostile input; `Session`
and `Framed` add the transport. `VOCAB_DATA` was added to the list as the
answer to `VOCAB_REQUEST`, and `SYMBOL_INFO` as the answer to
`SYMBOL_QUERY`.

A receiver recomputes the digest of a vocabulary from its content, runs its
test vectors, and refuses a vocabulary that collides with one it holds. An
`ACK` carries a hash of the decoded bytes so that the sender learns whether
both sides read the vocabulary the same way. Security of the channel is out
of scope and stated as such. See `specifications/protocol`.

## ADR-0023: Vocabulary identity is specified and checked twice

ADR-0006 left the digest defined by what the Rust serializer happened to
write. The canonical form is now written out rule by rule in KVF section 4,
and `conformance/kvf_digest.py` implements it from that text with the Python
standard library alone. CI runs it on recorded vectors, including files
written by the v0.2.0 release, and on vocabularies trained in the same job.

The existing form was specified as it is instead of moving to RFC 8785 or
CBOR, because a new form would change the digest of every published
vocabulary and invalidate every sequence that names one. Numbers with a
fraction are excluded from the canonical form; nothing in KVF needs them.

The review item "cache the verified digest" was checked before changing
anything: a loaded vocabulary carries its digest, `digest()` returns the
stored value, and the content hash is computed once at load. No change was
needed.

## ADR-0024: Training reads the corpus as a stream

Trainers already kept only counts, but the engine read every input file
into memory before training started, and the event trainer kept every
record. Now files are read one at a time, the event trainer keeps window
counts only, and `stream_bytes` reads a file in pieces cut after a line
feed, so one very large file is no different from many small ones.

Memory budget: one piece (or one file without `stream_bytes`), plus the
table of distinct chunks or windows, plus the merge trainer's working set.
The last two depend on how varied the data is, not on how much there is.
Measured figures are in `benchmarks/README.md`.

`stream_bytes` is part of the training configuration and is stored with the
vocabulary, because a piece is a record: it changes the corpus fingerprint,
and for training without a pre-tokenizer, where input is counted in fixed
windows that restart at each record, it can change what is learned. With a
pre-tokenizer on text, chunks do not cross line feeds and the learned tokens
come out the same. Checkpointing a training run and resuming it is not
implemented.

## ADR-0025: A compact vocabulary file that stores rules, not payloads

A 32,768-token text vocabulary is 3.3 MB as JSON. Most of that is the
payload of learned tokens, written out in base64, although each one is
determined by its merge rule. The compact form (`.kvfb`, KVF section 7)
stores the rule alone and rebuilds the payload on load; the same vocabulary
is 155 KB.

It is a second encoding of the same model, not a second format: the digest
is unchanged, it is computed over the rebuilt model, and a reader validates
that model in full, so a compact file is trusted no more than a JSON one.
JSON stays the interchange and inspection form.

Two costs were accepted. A small file can now describe a large table, so the
reader takes a budget for rebuilt payloads as well as for the inflated body.
And the form stores rules in id order only, so a vocabulary listing them in
another order is refused by the writer instead of being reordered, which
would change its digest.

Loading is not meaningfully faster: validation and the digest dominate, not
parsing. Measured figures are in `benchmarks/README.md`. A form that can be
used without rebuilding, for example memory-mapped, was not attempted. The
exchange protocol still sends JSON.

## Deferred

Registry service and signing, import from other tokenizer formats, training
checkpoints, typed fields and timestamps in events, entropy-coded
containers, comparison with Zstandard and with binary record formats, and
adapters for DNA, telemetry, mathematics and quantum circuits.
