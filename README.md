# KUTE: Kannaka Universal Tokenization Engine

KUTE trains, stores, exchanges and applies vocabularies for more than text:
bytes, structured records and event sequences go through the same engine, and
every vocabulary is a versioned, checkable file.

Status: v0.8, experimental. Formats and APIs may change before 1.0;
[docs/stability.md](docs/stability.md) says which, and how.

## Measured against Hugging Face `tokenizers`

Byte-level BPE, 32,768 tokens, trained on 90 MB of enwik8 and applied to
10 MB neither tool saw. Ten runs each, called from Python, on two machines
(medians; KUTE 0.5.0 against tokenizers 0.23.2):

| | 8-thread laptop | 24-thread desktop |
|---|---:|---:|
| train | 14.6 s against 27.0 s | 9.7 s against 10.8 s |
| encode the held-out text (tokenizers as a batch of lines) | 0.31 s against 2.99 s | 0.23 s against 0.89 s |
| tokens for the held-out text | 2,551,171 against 2,703,872 | the same |

So: 5.6% fewer tokens on both machines, training 1.1 to 1.8 times faster,
encoding 4 to 10 times faster. With more threads `tokenizers` closes most
of the training gap. Both decode back to the input exactly. The two tools
split text differently before merging, so this compares them as used, on
one dataset. The desktop run was made by the project's reviewer session
with the same script and wheels. Method, ranges, limits and the command to
rerun it:
[`benchmarks/README.md`](benchmarks/README.md#against-hugging-face-tokenizers-on-text-v050).

**With the split matched, most of the token difference goes away.** Given
KUTE's split pattern, `tokenizers` needs 2,571,668 tokens for the same
text: KUTE's 2,551,171 is 0.8% fewer, not 5.6%. The larger figure compares
defaults, and most of it is the split rather than the merging. Speed is
the difference that remains: in that matched run, on the laptop and with
the trainer released in v0.7, KUTE trained in 4.2 s against
29.9 s and encoded in 0.30 s against 3.01 s.

```
pip install kute-engine
```

## What works in v0.8

* A Rust workspace with a generic tokenizer interface and a registry, so new
  algorithms and adapters plug in without changes to the core.
* Algorithms: `bpe` (byte-level BPE, native Rust), `sequence-bpe` (BPE over
  named symbols such as events), and the baselines `bytes`, `char` and
  `dictionary`. `rustbpe` is available as a reference backend, built
  separately from `backends/rustbpe`.
* Adapters: `binary`, `text` (both byte-exact), `json`, `jsonl` (canonicalizing)
  and `jsonl-events` (keeps one field per record).
* KVF 0.1, the vocabulary file format, with content digests, optional test
  vectors and strict validation. See `specifications/kvf/KVF-0.1.md`.
* A CLI (`kute`), Python bindings, export to tiktoken's format, and a
  reproducible benchmark suite.
* Two token containers: plain varint ids, and a compressed form
  (`kute encode --compress`) that is smaller than gzip of the original on
  the suite's large text case. See `benchmarks/README.md` for where it is not.
  On a short message (a few dozen tokens) `--compress` makes the container
  larger, and the header naming the vocabulary is most of either form:
  see `examples/queensync`.
* Text encoding that matches tiktoken id for id on an exported vocabulary and
  runs faster than it on the same machine; long texts use every core.
* An experimental adapter contract for Holographic Resonance Medium (HRM)
  research. It is an interface only. See "What KUTE does not claim".

Added in v0.3:

* `kute discover`: recurring patterns in event streams, with support,
  evidence and a held-out check, no tokenizer needed. Measured on a public
  log dataset in `benchmarks/README.md`.
* `kute encode --unknown escape`: a frozen event vocabulary carries symbols
  it has never seen and still decodes exactly; `strict` refuses them.
* `kute evolve`: the next version of an event vocabulary, with new symbols
  or sequences appended and every existing token id unchanged.
* `kute peer serve` / `kute peer send`: two processes agree on a vocabulary
  by digest, transfer it if needed, extend it together and exchange encoded
  messages with a receipt. A peer takes nothing new from the other side
  unless told to (`--accept-vocabularies`, `--accept-proposals`), bounds
  what it will hold in all, and does not deliver a repeated message twice.
  With `--tls-cert`, `--tls-key`, an authority file and `--allow
  <fingerprint>`, the connection is TLS 1.3 with a certificate on both
  sides and only the named peers are served. A plain socket has to be
  asked for with `--insecure-plain`. A peer gives out vocabulary files,
  answers and translations only when told to. See
  `specifications/protocol`.
* `kute mcp --vocab file.kvf`: encode, decode, inspect, compare and
  discover as Model Context Protocol tools on standard input and output.
  Read-only: it works on the vocabularies it was started with and the data
  in each call, with bounded inputs and outputs. See `docs/mcp.md`.
* `kute keygen`, `kute sign`, `kute verify`: Ed25519 signatures on a
  vocabulary, over its digest, which does not change. Nothing checks
  them unless you run `kute verify` with the keys you trust, or serve
  with `kute peer serve --require-signature-from`; there is no
  registry. See `docs/signing.md`.
* `kute serve --studio`: a page, built into the binary and served on
  this machine only, for looking at vocabularies, encodings, recurring
  sequences in a corpus you give it, and report files. It shows what
  the service answers and lists what it cannot show. See
  `docs/studio.md`.
* `examples/queensync`: two scripted agents that train, observe,
  challenge and negotiate a vocabulary for their messages, with every
  byte counted, and what that showed (a general compressor with a
  dictionary was smaller).
* `kute predict`: how well an entity's recent events predict its next
  one, on held-out time, against two baselines, with calibration and
  drift. A measurement, not a model. See `docs/predict.md`.
* `kute shadow`: encode held-out data with the vocabulary in use and
  with a candidate, and approve the candidate only if it saves bytes
  after the cost of distributing it. See `docs/shadow.md`.
* `kute live`: discovery over events as they arrive, ordered by event
  time, in tumbling or sliding windows, with bounded memory, checkpoint
  and resume. See `docs/live.md`.
* `kute events pack|unpack|inspect`: complete event records (entity, time
  and every other field) kept exactly and stored by column. See
  `docs/events.md`.
* An experiment on layered vocabularies with local ids, which found no
  reason to add them: `docs/layered.md`.
* A first measurement of composed token representations (sum, rotated
  sum, binding) on real event logs, which did no better than exact
  counting: `docs/representations.md`.
* `kute signals pack|unpack|inspect`: a sequence of numbers kept exactly,
  or within an error bound that is checked on every value and stated in
  the file. Standard techniques; the contract is the point. See
  `docs/signals.md`.
* `kute serve --vocab file.kvf`: the same five operations over HTTP under
  `/v1/`, loopback only unless asked, no TLS or authentication of its own.
  See `docs/http.md`.
* `kute_discover::live` (library only, a prototype): discovery over a
  stream in state fixed in advance. Past its limits it approximates and
  reports by how much; within them it equals offline discovery.
* `kute train --stream-bytes`: training memory that does not grow with
  file size (227 MiB against 959 MiB on a 500 MB input), and a peak-memory
  figure in every training report.
* Vocabulary identity specified rule by rule and checked by a second
  implementation in Python (`conformance/`), with recorded vectors that
  include files written by v0.2.0.
* `kute train --no-test-vectors` for private corpora (the default from
  v0.4.0, where `--embed-test-vectors` opts in), `kute decode
  --max-units` for untrusted sequences, and a registry that refuses a second
  registration of a name.

Added in v0.4:

* A compact vocabulary file: name the output `.kvfb`, or run `kute convert`.
  Same digest, about a twentieth of the size on a 32K text vocabulary
  (155 KB against 3.3 MB). Every command reads either form.
* Typed events: `kute discover --symbol-field event --group-field device
  --time-field t` reads one event per line, separates the entities a file
  interleaves, and reports how long each discovered pattern takes.
* A comparison with Zstandard, xz and LZ4 on real data. On 1.8 million
  held-out log events the KUTE container is the smallest, 15 to 30 percent
  under the best of them; on English text it is 11 to 13 percent larger
  than `zstd -19` and `xz -9`. See `benchmarks/README.md`.

Added in v0.5:

* `kute train --resume-from small.kvf --vocab-size N` continues an earlier
  `bpe` or `sequence-bpe` run to a larger size and gives the same file one
  run would.
* The comparison extended to CBOR, MessagePack and Protocol Buffers.
* Per-document latency percentiles in every benchmark evaluation.
* Every training run re-encodes the start of its first records and fails
  if they do not come back exact, whether or not test vectors are stored.
* Python: `Engine.train(..., resume_from=, embed_test_vectors=)` and
  `kute.discover(records)`.

Added in v0.8 (each has a doc under `docs/` and a record in
`docs/decisions.md`; what an upgrade from v0.7 has to change is in
[docs/stability.md](docs/stability.md)):

* `kute serve` (HTTP, and `--studio`), `kute events`, `kute signals`,
  `kute live` as a command with `--end-on` and `--idle`, `kute shadow`,
  `kute predict`, `kute keygen/sign/verify`, `kute peer serve
  --require-signature-from`, and `examples/queensync`.
* The event and signal containers use Zstandard, which compiles C.
  Since 0.8.1 it is a feature (`zstd`, on unless turned off): without
  it the two libraries are Rust only and write zlib. The command line
  needs a C compiler either way, as it has since v0.7, for TLS.
* Results that went against KUTE, kept: on scripted agent messages zlib
  with a preset dictionary is smaller than KUTE tokens by a fifth;
  layered vocabularies gave no reason to add them; composed
  representations did no better than exact counting; on OpenSSH a longer
  past predicts the next event worse than the previous event alone; the
  signals container adds a checked contract and not bytes.
* Not built: a registry or federation, expiry or revocation of keys,
  learned representations.

Changed in v0.7 (the gates it was built against, with the evidence for
each and the limitations, are in [docs/release-0.7.md](docs/release-0.7.md);
what an upgrade from v0.6 has to change is in
[docs/stability.md](docs/stability.md)):

* Peers authenticate each other with mutual TLS, give nothing away unless
  told to, and hold to budgets on vocabularies, tokens, inbox and decoded
  bytes. The protocol number is 2.
* An evolved vocabulary records its parent by digest (`kute evolve-3`),
  and `kute evolve` warns when a step makes every token id a byte wider.
* The JSON adapters refuse duplicate keys and numbers that would not
  survive, instead of silently choosing.
* `kute mcp`, streaming discovery (`kute_discover::live`, a prototype),
  and `examples/log-archive`.
* Measured: evolution on drifting data, a source-code corpus, and the
  comparison with `tokenizers` under a matched configuration with one
  training per process.

Changed in v0.6:

* `kute evolve` builds an added sequence over the parent's own encoding, so
  the new token is one the encoder reaches. Before, most added tokens went
  unused. An evolved vocabulary now carries the generator `kute evolve-2`,
  the name of the rule, so releases that implement it agree on the digest.
  **v0.6 and v0.5 derive different children from the same step**, and
  peers on the two sides will see their digests differ.
* Measured: frozen, evolved and retrained event vocabularies on HDFS_v1
  (evolving did not help there), and the comparison with Hugging Face
  `tokenizers` above, including a one-thread run.

Version numbers here are the repository's own. Measured against the
development directive of November 2026, v0.6 covers its items D02 to D05
and D06 to D12 (D06 without a clean timing run); Studio, representation research and integrations
(D13 to D15) are not built. Of the requirements document's "v0.2" (REST
service, registry, MCP), v0.7 has the MCP server and v0.8 the REST
service; the registry is not built. Measured against the v0.7 development
specification, every item has a first pass in v0.8; three of them (layered
vocabularies, composed representations, QueenSync) ended in a negative
result.

## Quick start

Install from crates.io with `cargo install kute-cli`, or from PyPI with
`pip install kute-engine`. The Rust library is the crate `kute-engine`,
imported as `kute`. To build from source:

```
cargo build --release -p kute-cli
./target/release/kute sample-data --output data

kute train --input data/words-train.txt --id example:words --vocab-size 2000 --output words.kvf
kute encode --vocab words.kvf --input data/words-eval.txt --output words-eval.kute
kute decode --vocab words.kvf --input words-eval.kute --output words-eval.out
kute validate words.kvf
kute inspect words.kvf --longest 5
```

`words-eval.out` is byte-identical to `data/words-eval.txt`.

From Python:

```python
from kute import Engine

engine = Engine()
vocab = engine.train(source="data/messages-train.jsonl", algorithm="bpe",
                     adapter="jsonl", vocab_size=2048,
                     vocabulary_id="example:agent:messages")
tokens = engine.encode('{"from": "kannaka", "intent": "accept"}', vocabulary=vocab)
original = engine.decode(tokens, vocabulary=vocab)
```

Five worked use cases, each independent of the others, are in
`docs/use-cases.md`.

`examples/log-archive` is a separate application built on the published
Python package: it archives a public 70 MB server log with an exact round
trip and checks discovered event sequences on sessions it had not seen.
Its README gives the commands to reproduce it and says what the numbers
do and do not show.

## Layout

```
crates/kute-types       shared types: errors, identifiers, inputs, the vocabulary model
crates/kute-core        traits (Tokenizer, ModalityAdapter, RepresentationBackend), registry
crates/kute-bpe         byte BPE, sequence BPE
crates/kute-algorithms  bytes, char, dictionary
crates/kute-adapters    binary, text, json, jsonl, jsonl-events
crates/kute-vocab       KVF read/write/validate, compare, tiktoken export
crates/kute-discover    pattern discovery over symbol streams, held-out evaluation
crates/kute-protocol    vocabulary exchange between two peers
crates/kute             the engine (crate kute-engine): one entry point over all of the above
crates/kute-bench       evaluation harness and the benchmark suite
crates/kute-cli         the kute command
plugins/hrm             experimental HRM representation plugin
backends/rustbpe        rustbpe reference backend and the kute-ref command (not published)
bindings/python         Python package (PyO3, built with maturin)
specifications/         KVF 0.1, token envelope and .kute container, exchange protocol
docs/                   use cases, HRM contract, decision records
conformance/            independent implementation of the vocabulary digest
benchmarks/             how to run the suite, and recorded results
```

## Building and testing

```
cargo test --workspace                       # no Python needed
cd backends/rustbpe && cargo test            # needs Python; see below
cd bindings/python && maturin develop --release && pytest tests
```

The rustbpe backend embeds a Python interpreter, because rustbpe's only
training entry point takes a Python iterator. It needs Python at build time
and at run time. On Windows, set `PYO3_PYTHON` to the interpreter and put its
directory on `PATH`. It depends on a pinned git revision of rustbpe, since
the crates.io release uses an older PyO3; crates.io does not accept git
dependencies, so this backend is a separate crate that is not published.
Its `kute-ref` binary is the `kute` command with `--algorithm rustbpe`
added.

## Losslessness, stated per component

Every adapter and algorithm declares whether it is lossless, and the
declaration is stored in each vocabulary.

* `binary` and `text` adapters with any built-in algorithm: decoding returns
  the original bytes exactly.
* `json` and `jsonl`: decoding returns canonical JSON (sorted keys, compact).
  The data is the same; the bytes are not.
* `jsonl-events`: only the chosen field survives. It is for finding recurring
  event sequences, not for storing logs.

## What KUTE does not claim

* **No compression claims beyond what the benchmarks show.** The suite reports
  full serialized size, with and without the vocabulary file, next to gzip.
  The compressed container beats gzip on 10 MB of held-out Wikipedia text
  when the vocabulary is already shared, and loses to it on small binary
  records. On small datasets the vocabulary costs more than it saves. Most
  suite datasets are synthetic and are labeled so.
* **No claims about HRM.** `plugins/hrm` maps token content to deterministic
  phase vectors so that HRM experiments have a stable interface. Whether those
  representations are useful for anything is an open research question that
  this repository does not test.
* **No semantic equivalence.** Vocabulary comparison matches tokens by
  content. It says nothing about meaning.
* **No privacy.** A vocabulary can reveal frequent strings from its training
  data, and a `.kute` file is decodable by anyone with the vocabulary.

Not in v0.2: registry service, signatures, streaming, import from other
tokenizer formats, and adapters for DNA, telemetry, mathematics and quantum
circuits.

## License

Licensed under either of Apache License 2.0 (`LICENSE-APACHE`) or the MIT
license (`LICENSE-MIT`), at your option. See `CONTRIBUTING.md` for how
contributions are licensed.
