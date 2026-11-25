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
private corpus that publishes the data. `embed_test_vectors` controls it and the choice is recorded in the
training provenance. In v0.3 the default was on (CLI `--no-test-vectors`
turned it off). From v0.4.0 the default is off and `--embed-test-vectors`
turns it on (issue #2): a vocabulary file is what gets published and
exchanged, and the person a default harms is the one who did not read
about it. A vocabulary without vectors still carries its digest; what it
loses is the encode check that `kute validate` runs. Learned tokens are frequent fragments of the corpus and
remain visible in any vocabulary; that is inherent and is not hidden by this
switch.

Since v0.4.0 a training run through the engine also encodes and decodes the
start of its first four records (up to 64 KiB or 4,096 symbols each) and
fails if they do not come back exact. This check runs whether or not vectors
are stored, and keeps nothing (issue #2).

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

After v0.5.0 the step changed in one respect. An added sequence used to be
built as a chain of merges over its bare symbols. The encoder applies the
parent's merges first, so where the parent already joined part of the
sequence the new merges never applied and the added token went unused. The
step now tokenizes the addition with the merges already present and chains
the tokens that remain. On the HDFS_v1 events, a vocabulary made only of
discovered sequences then encoded the held-out events in 399,211 tokens
where the old step gave 831,731 (benchmarks/README.md). Cost: the same
parent, additions and version give a different child, and a different
digest, than under v0.5.0 and earlier. Two parties must run the same side of
this change to agree on an evolved vocabulary; they find out by comparing
digests, as before.

The reviewer then found that a new merge has to be applied to the whole
addition before the next is chosen, since the encoder applies it that way
(an addition such as `A A A A` otherwise leaves unused tokens), and that the
child carried `kute <release>` as its generator. The generator is part of
the digest, so two releases could never agree on an evolved vocabulary even
when they ran the same step. An evolved vocabulary now carries
`kute evolve-2`: the name of the rule, which changes only when the rule
does. Releases up to v0.5.0 implement rule 1 and do not agree with rule 2.

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

## ADR-0026: Typed events keep time and grouping beside the symbols

Until now an event was a name. The `jsonl-events` adapter kept one field
per line and dropped the rest, so a file in which two machines' events are
interleaved was read as one sequence, and nothing downstream could ask how
long anything took.

`EventStream` (in `kute-types`) holds, per entity, the event names and a
timestamp for each, with the `EventSchema` that says which fields they came
from, what a tick means, and under what name and version that reading is
known. The schema is hashed into the stream's fingerprint: the same file
read two ways is two streams.

The model is deliberately small. It adds two things, grouping and time,
because there is a consumer for each: discovery runs per entity, and
`kute_discover::timing` reports how long each discovered pattern takes.
Tokenizers and discovery still take plain symbols; the times stay aligned
with them by position. Numeric values, units of measurement and
relationships between entities are not modelled, and timestamps are not
carried in token containers or vocabularies. Timestamps are whole numbers
of a declared unit; a fractional or textual time is an error, not a
conversion.

Measured on real data in `benchmarks/README.md`.

## ADR-0027: Adapters reconstruct with the configuration they ingested with

`jsonl-events` read the field named in its configuration and wrote the
default name, `event`, on the way back, so its own output could not be fed
to the same vocabulary (issue #3). `reconstruct` now receives the adapter
configuration, which the vocabulary already stores. This changes the bytes
that decoding produces for event vocabularies trained with a `field`
option: `{"tag":"A"}` where earlier releases wrote `{"event":"A"}`. Token
ids, files and digests are unaffected, and the two recorded `.expected`
files for event sequences were updated to the new output.

The JSON adapters' descriptions said values survive. They now say what
happens: numbers beyond a 64-bit integer or a double are rounded to a
double, and of repeated keys the last is kept. Refusing such input instead
is not implemented.

Token ids in the plain container must be in their shortest varint form;
padded forms are refused (issue #4).

## ADR-0028: A vocabulary is its own training checkpoint

A `bpe` or `sequence-bpe` run can continue from a vocabulary trained earlier on the same
corpus with the same settings and a smaller size (`TrainingConfig::
resume_from`, `kute train --resume-from`). The corpus is read again, the
earlier merges are applied to it, and learning goes on. Which pair is merged
next depends only on the state of the corpus under the merges so far, so the
result is the vocabulary one run to the new size would give: same tokens,
same digest, same file. The tests check that, with and without a token
length limit.

No separate checkpoint format was added. The vocabulary already records its
merges, corpus fingerprint and settings, and a resumed run is refused when
the fingerprint or a setting that affects merging differs.

A resumed run trusts the merges it is given. The checks establish that the
earlier vocabulary claims the same corpus and settings, not that its merges
are what training on that corpus produces; a file edited to pass them would
steer the result. Resume only from a vocabulary you trained or trust.

What this does not give: a run does not write intermediate files by itself,
so to survive an interruption it has to be run in stages. Each stage reads
the corpus again; on 90 MB of text that was about 7 of the 17 seconds a
whole run takes. `sequence-bpe` resumes the same way; the baseline
algorithms cannot resume.

## ADR-0029: The JSON adapters refuse what canonicalizing would change

The `json`, `jsonl` and `jsonl-events` adapters parse a document and write
it back in one canonical form. Two kinds of input have no single reading:
an object that repeats a key, and a number that does not survive (an
integer beyond 64 bits, a decimal with more digits than a double keeps).
Until v0.6 the last value of a repeated key was kept and such numbers were
rounded, both silently, so two different documents could become the same
bytes and the same tokens.

From v0.7 these inputs are refused with an error that names the key or the
number. Keys are compared as decoded, so `"a"` and its escaped spelling
count as one. A decimal is accepted when the double it parses to, written
back, has the same significant digits: `2.50`, `25e-1` and `0.1` pass,
`0.1000000000000000055` does not. The adapter option `strict=false`
restores the earlier behaviour for data that cannot be fixed at the source.

Canonical output is unchanged for every input that is still accepted, so no
vocabulary digest or recorded vector changes. What changes is that some
inputs accepted before now fail; that is the migration, and the option is
the way through it. Not covered: numbers are still held as doubles or
64-bit integers, so there is no exact decimal type, and `-0` is kept as a
negative zero as before.

## ADR-0030: The runtime's vocabulary is sealed where it is loaded

A `Vocabulary` value is plain data with public fields, and its `digest()`
returns the digest that was recorded. Code can change a field afterwards
and the value will go on reporting the old digest. Making the fields
private would break every caller that builds or inspects vocabularies, so
the value stays open and the boundary is put where it matters: a tokenizer.

`Registry::tokenizer_for`, which `Engine::tokenizer` and every binding go
through, now checks after the algorithm's own `load` that the vocabulary
the tokenizer holds carries a digest and that the digest is the hash of
its content. A tokenizer hands out only shared references to what it
holds, so from then on the two cannot part. The five built-in algorithms
already refused a changed vocabulary; the check makes that a property of
the runtime instead of each algorithm, plug-ins included.

What this costs: one more hash of the vocabulary per load, about 17 ms for
32,768 tokens on the development laptop (191 ms against 174 ms for a whole
`kute encode` of a short file). It is paid once per tokenizer, not per
message. In Python a `Vocabulary` object is frozen and compiles its
tokenizer on first use, so one object is a persistent session.

What it does not do: an unsealed `Vocabulary` passed around as a value is
still only as trustworthy as the code that made it, and `digest()` on it
is a record, not a proof. Code that receives one from outside should call
`validate()` or load it.

## ADR-0031: Exchange protocol 2: deny by default, budgets in all, the rule named, repeats answered once

The directive for v0.7 asks that the exchange be safe to put in front of a
peer that is not trusted. Four parts of that need no new dependency and
are done together as protocol number 2:

* **Deny by default.** `Policy::default()` took vocabularies from the other
  side; it now takes neither vocabularies nor proposals. `kute peer serve`
  has `--accept-vocabularies` in place of `--no-new-vocabularies`.
* **Budgets in all.** Per-item limits did not stop a peer from being filled
  one acceptable item at a time. There is now a limit on vocabularies held
  and on tokens over all of them, applied when a vocabulary is announced,
  again when it arrives, and to a proposal before it is derived; and a
  limit on the messages and bytes waiting in the inbox, past which a
  message is answered `busy` and not delivered.
* **The evolution rule is negotiated.** v0.6.0 changed the rule, and peers
  on the two rules found out only when their digests differed. The rule is
  now listed in `CAPABILITIES` and named in `SYMBOL_PROPOSE`; a mismatch is
  refused before derivation on both sides.
* **A delivery contract.** A repeated message id with the same container
  gets its first receipt again and is delivered once; with a different
  container it is refused. The memory of ids is a bounded window.

The protocol number went up instead of the new fields being optional,
because a number 1 peer would send proposals without a rule and expect
vocabularies to be taken by default; refusing it at `HELLO` says so at once.

Not done here, and the reason the protocol is still not for untrusted
networks: authentication and encryption (so policy is the same for every
peer), limits on connections, rate and CPU time, and lineage by parent
digest in place of the copied training record. Those are the next steps of
the same work package.

## ADR-0032: Peers are authenticated by mutual TLS, and policy follows the certificate

The exchange needed to know who it is talking to before any of ADR-0031's
policy could differ between peers. The choices were to add signatures to
the protocol's own messages or to run it over an established authenticated
transport. The second was taken: TLS 1.3 with a certificate on both sides,
through `rustls` with its `ring` provider. It gives encryption, replay
protection within a connection and authentication of both ends, and it is
not something this project should write itself.

`kute_protocol::tls` is behind the crate feature `tls`, on by default. It
builds the two configurations, completes the handshake, and returns the
stream together with the identity of the other side: the SHA-256 of its
certificate. `Access` maps identities to policies and refuses anyone not
listed, so being issued a certificate by the trusted authority is necessary
but not sufficient. The name in `HELLO` stays a label.

Cost: `rustls` and `ring` are new dependencies of the protocol crate and of
the CLI (`ring` builds C and assembly). The engine crates do not depend on
them. `rcgen` is a test dependency only; no certificate is shipped.

`kute peer serve` and `kute peer send` take the certificate, key and
authority files, `--allow <fingerprint>` names who may connect, and `kute
peer fingerprint` prints the identity of a certificate. Without those
options the commands still open a plain socket, and say so on stderr. A
connection announces its own end (TLS `close_notify`) when dropped, and one
that ends without that notice is an error to the reader, so a cut
connection is not mistaken for a finished one.

Not done: there is no revocation beyond removing an identity from the
list, and limits on connections and rate are still the caller's.

## Deferred

Registry service and signing, import from other tokenizer formats,
checkpoints written during a run, event values, units and relationships,
timestamps in token containers, entropy-coded containers, and
adapters for DNA, telemetry, mathematics and quantum circuits.
