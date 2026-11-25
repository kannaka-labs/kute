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

Found later by the property tests over damaged input (gate AC-712): the
JSON library's default number parser is fast and not always exact, so a
number such as `1e61` could be written in canonical form, read back as the
neighbouring double, and written differently. Canonical form was not a
fixed point for those inputs. The library's exact parser (`float_roundtrip`)
is now on for the whole workspace.

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

Identity is the certificate, not the key: a renewed certificate for the
same key has a new fingerprint and has to be allowed again. Removing a
fingerprint is the only revocation; no revocation list is checked.

Not done: limits on connections and rate are still the caller's, and
`kute peer serve` still serves one allowed connection and exits.

## ADR-0033: A peer gives nothing away by default, and a plain socket has to be asked for

From the security review of ADR-0031 and ADR-0032 (pull request 20).

* A compressed container can hold thousands of times its size in ids, and
  the peer inflated it before any limit. Containers from the other side
  are now refused on the token count in their header
  (`TokenSequence::from_bytes_bounded`), under `max_message_tokens`
  (16 Mi by default) and the unit limit.
* The default policy took nothing but gave everything: any ready peer
  could fetch a held vocabulary by digest, ask about its symbols and have
  sequences translated. `Policy` now has `give_vocabularies`,
  `answer_queries` and `translate`, all off by default, and
  `vocabularies`, the digests the other side may use at all. A vocabulary
  that is held but not listed is answered exactly as one that is not
  held. Delivering a message under a usable vocabulary is the one thing a
  default peer does.
* A proposal whose id and version are already held with other content is
  answered `VOCABULARY_CONFLICT`. It is still derived first: a repeat of
  the same proposal must be accepted again, and only the derivation tells
  the two apart. The cost is bounded by the budget checks before it.
* The inbox budget counts what the inbox keeps: decoded bytes, ids at four
  bytes each, and the container.
* `kute peer serve` with TLS refuses a connection that fails the
  handshake, stalls in it, or is not allowed, and waits for the next one;
  before, anyone who could reach the port could end the listener. Reads
  and writes time out (`--timeout`, 30 seconds by default).
* Without the TLS options, `kute peer serve` and `kute peer send` refuse
  to run unless given `--insecure-plain`.

This changes the behaviour of protocol 2 as first written on this branch;
it has not been released, so the protocol number stays.

## ADR-0034: An evolved vocabulary names its parent by digest only

Evolution rule 2 wrote the parent's whole training record into the child
(`parent_training`), and the parent's held its own parent's, so every
generation carried all the earlier ones. Rule 3 (`kute evolve-3`) writes
`evolved_from: {version, digest}` and the additions, nothing else. The
parent's record is in the parent, which the digest identifies.
`vocab::parent_digest` and the `parent` field of a summary read it.

Provenance is inside the digest, so the same step gives a different child
under rule 3 than under rule 2, and the rule has a new name. A peer on
v0.6.0 already cannot talk to this one (protocol 1 against 2); the rule
name makes the difference explicit for anything that compares digests.
Vocabularies evolved under rule 2 stay valid files and load as before;
`parent_digest` reads them too.

Not done: nothing checks that the named parent exists or that the child
really follows from it. Checking needs the parent and the additions, and
both are available to whoever holds the parent: derive and compare.

## ADR-0035: The MCP server is read-only, and lives in the CLI

`kute mcp` offers encode, decode, inspect, compare and discover as Model
Context Protocol tools on standard input and output (`docs/mcp.md`). The
specification asks that training and publishing need explicit permission
and quotas; here they are not offered at all, which is the strictest form
of that and can be relaxed later with a design of its own. A tool call
touches only the vocabularies the server was started with and the data in
the call: no path is ever taken from a caller, so there is no file access
to police.

The protocol is small enough (four methods, line-delimited JSON) that it is
written directly on `serde_json` and adds no dependency. It is a module of
`kute-cli` and not a crate: one more crate to publish for 300 lines did not
seem worth it, and the tools are the CLI's operations under other names,
on the same engine.

Not done: the HTTP transport, resources and prompts, and a REST surface.

## ADR-0036: Streaming discovery holds a fixed amount of state and reports what it approximated

`kute_discover::live::Live` takes events one at a time (entity, symbol)
and counts each entity's contiguous sequences, in state bounded by its
parameters: symbols known by name, sequences counted at each length,
entities whose recent symbols are remembered. `state_bytes_bound` is a
function of the parameters alone, and the report carries the state in use
beside it.

Counting uses the Misra-Gries summary because its error is one-sided and
known exactly: a count is never above the truth, and the number of times
the table was decremented is the most it can be short. Symbols past the
limit are folded into one, and entities past the limit lose their recent
symbols, least recently seen first. Those two lose sequences before they
are counted; the report says how many events were folded and how many
entities evicted, and gives no bound on what that cost.

While no limit is reached the result equals offline `discover` for
contiguous patterns (tested: same sequences, support and cohesion). State
can be saved and restored; a restored run continues to the same state as
an uninterrupted one, and a checkpoint is checked against its own bounds
before it is loaded.

This is the prototype the v0.7 specification asks for (C2a, AC-708), not
C2: there are no gapped pairs, no event time or late-event policy, no
windows that forget, no transport, no CLI, and the 24-hour replay with
recorded throughput has not been run. The test streams are synthetic.

## ADR-0037: The HTTP service is the MCP server's operations behind a hand-written, bounded listener

The v0.7 specification lists a REST surface beside MCP (E1): versioned
encode, decode, discover, inspect, compare and capabilities with bounded
payloads. `kute serve` provides it by calling the same function the MCP
server calls for each tool, so the two cannot drift apart, and adds only
the transport.

The transport is written on `std::net`, about 250 lines, instead of
taking an HTTP framework. The service needs one request per connection,
JSON in and out, and nothing else; a framework would bring an async
runtime and defaults (keep-alive, pipelining, chunked bodies, generous
header limits) that each need a bound found and set. Written out, every
bound is visible in one file: workers, queue, read deadline, header block,
body length checked before reading. What it gives up is everything a
framework does well, and it must never grow into one: if TLS, HTTP/2 or
streaming are wanted, the answer is a proxy in front or a real library,
not more code here.

It has no TLS and no authentication, so it binds loopback by default and
needs `--insecure-remote` otherwise, in the manner of ADR-0033. Loopback
is reachable from a browser, so it also refuses a `Host` that is not a
loopback name (DNS rebinding) and a body not declared as JSON (a form
post from another origin), and sends no cross-origin headers.

Not offered: training and jobs, which the specification lists for REST
with "explicit permission and quotas". They write files and run for
minutes; they need the permission model first and are left out until it
exists. Time spent inside one operation is not bounded, and there is no
per-client rate limit.

## ADR-0038: Complete events are stored by column, in a container of their own

The v0.7 specification asks for an event profile that keeps identity,
time, attributes and numeric values, where a token sequence keeps only
the names (C1). `kute-events` packs JSON Lines records into a `KUTE-EV1`
container and unpacks them exactly; `docs/events.md` describes it.

It is a separate container, not an extension of the token container. A
token container is one sequence of ids under one vocabulary; a record
has several things of different kinds, and they compress when each is
stored with its like. Forcing them into one token stream would mean
inventing ids for timestamps and values.

"Exactly" means canonical JSON, the same rule as the `jsonl` adapter and
through the same code: the records and their values come back, key order
and spacing do not, and the header says whether the source was already
canonical, in which case the bytes are the same. Events stay in source
order; the entity grouping that helps compression is undone on reading.

Columns were first compressed with zlib, because it was already a
dependency. Review showed what that cost: on the OpenSSH records the
plainest column layout through xz was 22% smaller than the container.
Columns are now Zstandard frames by default (level 19, a window that
covers the column up to 128 MiB), which makes the container 8% smaller
than that rival; zlib remains as `--codec zlib` for a container
readable with nothing else. Every column names its codec. Zstandard is
the workspace's first C dependency, through the `zstd` crate, which
builds the library from bundled source: building `kute-events` or the
command line now needs a C compiler. It was chosen over xz for its
speed in both directions at nearly the same size. The other crates do
not depend on it.

Deliberately absent: the event names are not tokenized with a trained
vocabulary. The container needs no vocabulary to read, which removes a
failure mode, and whether sequence tokens would beat zlib on the grouped
name column has not been measured. Also absent: streaming writing,
reading part of a container, units of measurement, and relationships
between entities other than the one group field.

## ADR-0039: Live discovery orders by event time behind a watermark and forgets by panes

The prototype of ADR-0036 counted events in arrival order and never
forgot. `kute_discover::watch::Watch` and `kute live` add what the
specification's C2 asks for; `docs/live.md` describes the behaviour.

Order: arrivals wait in a bounded buffer and are counted in order of
(time, arrival) once the watermark, the latest time minus a configured
lateness, passes them. Ties go by arrival so the result is a function of
the arrivals. The late-event policy is to drop and count. The
alternatives were to count late events out of order, which makes a
sequence depend on delivery, or to hold windows open for them, which
makes memory depend on the stream. When the buffer is full the earliest
event is counted early and the watermark moves; that trades lateness
tolerance for the memory bound, and is counted.

Forgetting: time is cut into panes and each pane has its own
Misra-Gries tables; a window is the last k panes, and a report adds
their counts. Summaries of this kind add, and their undercount bounds
add with them, so a sliding window needs no subtraction, which such a
summary cannot do. The price is k tables and a merged copy at each
report. One pane gives tumbling windows.

Reports are driven by events, not by a clock: a pane closes when an
event of a later pane is counted. A stream that goes quiet reports
nothing until it speaks again or ends. A timer would need a second
source of time and make replays differ from live runs.

Transport is a file or a pipe, read synchronously, which is the
backpressure. Reports are delivered at least once across a restart.
NATS and MQTT adapters, which the specification lists as optional, are
not built. Durations of patterns and gapped pairs remain offline only.

## ADR-0040: A candidate vocabulary is judged in shadow, on net bytes, and nothing is approved automatically

Measurements on HDFS and on synthetic drifting data showed that an
evolved vocabulary can emit fewer tokens and larger output. `kute shadow`
(specification D1) encodes held-out inputs with the vocabulary in use
and with a candidate and gives a decision with its evidence;
`docs/shadow.md` describes the terms.

The quantity judged is net bytes: compressed containers, scaled by the
traffic the candidate is expected to carry, less the cost of sending
the candidate's file. Token count is reported and not judged, because
it is not what is stored or sent. The horizon and the number of
distributions are the caller's to state; the command cannot know them,
and the default (the inputs once, one distribution) is deliberately
unfavourable to the candidate.

The command decides and does nothing else. Publishing, monitoring and
rolling back are existing commands and the properties vocabularies
already have (immutable, identified by digest, a child never
invalidating its parent), written down as a procedure rather than built
as a controller. An automatic loop would need a policy for who may
approve, which belongs with the permission model that does not exist
yet.

Memory, task quality and predictive utility are not measured.

## ADR-0041: Predictive value is measured against baselines on held-out time, by a predictor kept simple on purpose

Discovery reports sequences that recur. Whether they help anticipate
anything is a separate question (specification D2). `kute predict`
answers it with a measurement; `docs/predict.md` describes it.

The cut is in time, not at random: a random split puts the future of an
entity in training and its past in test. The comparison is between
three predictors of one family that differ only in how far back they
look, so that the difference is the value of the longer past and not of
a cleverer method. Scores are log loss in bits, top-1 accuracy and
calibration; entities unseen before the cut are scored apart; the
held-out time is scored in parts to show drift; differences carry
intervals from resampling entities.

The predictor is a smoothed n-gram with backoff. It is not offered as a
model. A stronger one would give larger numbers and tell less about the
data, since its skill and the data's structure could not be told apart.

One change was made after seeing held-out results: the first runs on
HDFS and OpenSSH showed the first event of an entity given a confidence
near 0.15 and being right 93% of the time, because "nothing came
before" was not a context. A start marker was added. It is a fix to the
model's definition and it was prompted by test data, so those two
datasets are not clean evidence for it.

Repeated testing across runs is not controlled and cannot be from
inside one command. Association is not cause, and the report says so in
its output.

## ADR-0042: QueenSync is an example made of released commands, and its result is published as measured

The specification's E2 asks for two agents that observe their traffic,
discover procedures, judge candidate tokens in shadow, negotiate a
vocabulary and exchange encoded messages. `examples/queensync` does
that with a script driving `kute train`, `peer`, `discover`, `evolve`
and `shadow`. Nothing was added to the engine for it: if the flagship
needed private hooks, an outside developer could not build the same.

Agents are scripted and the workload synthetic. Calling a language
model would cost money and would not change the bytes measured. The
report says that model tokens are not measured and why they would not
change.

Messages are carried as symbols: one for each brace, comma and key, one
for each character of a value. Whole values as symbols was tried first
and failed before producing a result: every task id became a vocabulary
entry.

The measured result is that the loop works (1,000 of 1,000 tasks, novel
message kinds carried by escape, the candidate rejected by shadow and
the rejection borne out), and that zlib with a preset dictionary carries
the same tasks in about a third of the bytes of the KUTE container. The
settings were fixed before the result and were not tuned after it. The
example's README states this. A flagship that hid it would be an
advertisement.

Not built: a signature on the vocabulary (B8), resuming after a dropped
connection, per-message latency.

## ADR-0043: Studio is a page inside the binary, over the same HTTP interface, showing only what a backend answers

The specification's F2 asks for a local-first workbench in which each
view consumes actual backend output and exposes provenance and
limitations. `kute serve --studio` serves one; `docs/studio.md`.

It is three static files compiled into `kute`, calling `/v1/` like any
other client. No second interface was added for it: what Studio can
show is exactly what an outside developer's page could show. No
framework and no build step, so the page that ships is the page in the
repository.

Views exist for what the service answers (runtime, vocabulary, encode,
compare, discover) and for report files from the command line. Peer
status, event timing, running benchmarks and representation experiments
have no backend operation and are listed as absent with the reason. A
view with invented or sample data was the alternative and is what the
specification rules out.

The service is unauthenticated, so a browser page on it is a new way
in. It is off unless asked for, refused on any address but loopback,
and carries a content security policy of `default-src 'none'` with
scripts, styles and requests allowed from its own origin only, which is
why the script and style are separate files rather than inline. Text
from the service or a file is only ever set as text. The existing host
rule covers the page as it covers the interface.

The page was driven in a headless browser through every view once.
That check is not automated: it needs a browser, which the test suite
does not assume. The automated tests cover what the service sends.

## ADR-0044: A vocabulary is signed over its digest with Ed25519, and trust is whatever keys the verifier names

KVF 0.1 reserved a `signatures` list beside the digest and defined no
scheme. `kute keygen`, `kute sign` and `kute verify` define one;
`docs/signing.md`. It is the first step of the specification's B8.

What is signed is a fixed context line and the digest. The digest
already covers the whole content, so the signature does too, and the
context line keeps it from being a signature of anything else. The
verifier computes the digest from the content. Signatures stay outside
what the digest covers: a signed vocabulary keeps its digest, so nothing
that names it by digest changes, and signatures can be added by several
parties without each invalidating the others. `seal` now keeps the
signatures when the digest is unchanged and drops them when it is not;
before, every save dropped them.

Ed25519, by `ring`, which is already in the build for TLS. One
algorithm and no negotiation: an unknown algorithm is reported and
counts for nothing.

Trust is not modelled. `kute verify` takes the public keys the caller
trusts and reports against those; a key found in the file proves
nothing and none is stored there. A registry, key chains, expiry and
revocation are the rest of B8 and need a design of who may vouch for
whom, which this does not attempt.

Nothing enforces a signature: the engine, the peer protocol and the
service keep signatures and check none. Requiring one before a peer
accepts a vocabulary is the obvious next use and is not built. The
private key is an unencrypted file.

## ADR-0045: A signal is stored as numbers under a stated contract, exact or bounded, and the bound is checked per value

Status: accepted, experimental.

The specification asks for signals (F3): numeric sequences, delta
encoding, quantization, error bounds. Symbols are the wrong unit for a
meter reading, so this is a separate crate (`kute-signals`) and a
separate container (`KUTE-SG1`), not an adapter that turns numbers into
tokens.

Decided:

* Two contracts, named in the header: `exact` (same bits, including NaN
  payloads and the sign of zero) and `bounded` (every finite value within
  an absolute `max_error`; values that are not finite exactly).
* The bound is enforced per value while packing: a value whose
  reconstruction would miss it is kept as raw bits. Rounding arithmetic
  is not trusted to stay inside the bound near the edges of a step or at
  large magnitudes.
* `pack` reads its own container back and compares against the contract
  before returning. The header carries the largest error incurred and a
  SHA-256 of the values a reader must obtain; `unpack` refuses anything
  else.
* The step of a bounded container is stored as the bits of an `f64`, so
  the reader multiplies by the same number the writer did.
* The method is ordinary: decimal scaling or uniform steps, differences
  of order 0 to 2 chosen by trial, varints, zstd. A bounded container is
  written in steps and as the values are, and the smaller kept (found on
  real data: a bound finer than the data's resolution made steps cost
  more than the values).
* zstd only. The events container's choice of codec is still open with
  the maintainer; whatever is decided there applies here.

Not decided, not built: relative bounds, bounds on derived quantities,
many signals in one container, streaming, any predictor or transform.

Measured on one real data set (`docs/signals.md`): 0.2% smaller than the
same differences written as varints under zstd by hand, which is to say
the container adds nothing in bytes.
Purpose-built float compressors were not run. The reason to have it is
that the file says what it promises and a reader can check it.

## ADR-0046: Vocabularies stay flat; layered scopes with local ids were measured and did not earn a format

Status: accepted. An experiment was run; nothing was built into the
engine.

The specification (D3) asks for an investigation of base, domain,
environment and session scopes with negotiated local ids and reversible
fallback, compared with a flat vocabulary, and says KVF is not to be
replaced until evidence supports it.

Measured (`docs/layered.md`, `benchmarks/layered_vocab.py`): a
Wikipedia vocabulary used on Python source, with tables of local ids at
domain and session scope, against a flat vocabulary trained on the
domain. Real data, one domain, one run.

* Local ids brought the base vocabulary's token bytes to within 2.5% of
  the flat vocabulary's, and after zlib to within 0.4%. They cannot
  change the token count, which stayed 46% higher.
* A general compressor closed the same gap without any table.
* The largest gain found had nothing to do with layers: ranking a flat
  vocabulary's ids by frequency took 19% off its token bytes.

Decided: KVF stays as it is. No layer format, no local-id negotiation
in the protocol. Derivation from a parent (`kute evolve`, proposals
between peers) remains the only relation between vocabularies.

Left open, not decided: whether token ids should be assigned or carried
in frequency order. That would change every digest and the tiktoken
export, and would want its own measurement on event vocabularies, where
most ids are one byte already.

## ADR-0047: A peer may require a signature on a vocabulary it is sent; the protocol only carries the question

Status: accepted, experimental.

ADR-0044 left signatures checked by nothing but `kute verify`. The next
step of B8 is a peer that refuses an unsigned vocabulary.

Decided:

* `kute-protocol` gains no cryptography. `Peer::require_vocabulary_check`
  takes a function from a vocabulary to accept or a reason; it is called
  on a vocabulary received, after its digest is recomputed and matched
  and before it is held. The CLI supplies the Ed25519 check from the
  same code as `kute verify`.
* While a check is set, proposals are refused. A proposal derives a
  vocabulary on the receiving side that no signer has seen; accepting it
  would let the other side put an unsigned vocabulary into a peer that
  requires signed ones. The CLI refuses the two options together.
* Vocabularies the operator loads are not checked.
* The requirement is per peer process, not per allowed fingerprint. A
  per-fingerprint set of trusted keys would belong in `tls::Access`
  and is not built.
* The wire format is unchanged: the refusal is the existing `Refused`
  error with the reason, so an older sender understands it.

Not built: a registry, key distribution, expiry, revocation, threshold
rules (two of three keys), signing a proposal.

## ADR-0048: Composed representations are identified, toleranced and measured before anything is built on them

Status: accepted, research. A negative result is recorded.

The specification (F1) asks for research into compositional
representations with the phase-hash mapping kept as the baseline, an
identity for every representation, portable tolerances, and published
measurements including negative ones.

Decided:

* The three standard compositions (bundle, permute-bundle, bind-chain)
  live in `kute_hrm::compose`, outside the engine. No command, no
  service operation, no container carries a vector.
* A composition's identity is a string naming algorithm and version,
  dimension, configuration, and the base mapping and its version.
  Vectors under different identities are not compared.
* Portable tolerance: similarities of unit-energy vectors agree within
  1e-4 between implementations. Pinned by a test that runs on two
  platforms in CI.
* Measurements compare against exact baselines that use no vectors
  (counts, positions), and ties are split, after a first scoring that
  counted ties as misses made the vectors look better than the exact
  thing they approximate.

Measured (`docs/representations.md`): on windows of two real logs,
bundle equals counting and permute-bundle equals counting by position;
bind-chain is a hash. Nothing beat exact counting.

Not built, and the reason the question stays open: learned or
co-occurrence representations, where similar tokens get similar vectors;
any associative or predictive medium; sequence completion;
generalization. Until one of those is built and measured against these
baselines, no claim about the usefulness of these representations is
made.

## ADR-0049: An entity silent past a stated time starts over, and is let go without loss

Status: accepted.

ADR-0039 forgot entities only by eviction, and an ending event was
added for streams that state an end. Streams with reused ids and no
dependable ending event (OpenSSH process ids) had neither: old
sequences were joined to new ones, and finished entities held places.

Decided: `idle_ticks` (`--idle`). An entity whose last counted event
is more than that before its next starts a new sequence. When places
run out, entities silent that long go first and are not counted as
evicted, because the rule would have joined nothing to them. Measured
in event time, in time order, like everything else in the watch; no
timer. A checkpoint with a limit carries each entity's last time, and
one without is unchanged byte for byte.

Rejected: a timer on the wall clock (a replay would differ from the
live run); counting idle removals as evictions (it would mark exact
reports inexact).

Measuring it found that the OpenSSH event file had been dated wrongly
across a year boundary. Results that used its times were redone:
the prediction report (its sign changed) and the live figures. Results
that use only the order of lines (the representation windows) stand.

## Deferred

Registry service and signing, import from other tokenizer formats,
checkpoints written during a run, units and relationships between
entities, timestamps in token containers, entropy-coded containers, and
adapters for DNA, telemetry, mathematics and quantum circuits.
