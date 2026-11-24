# KUTE: Kannaka Universal Tokenization Engine

KUTE trains, stores, exchanges and applies vocabularies for more than text:
bytes, structured records and event sequences go through the same engine, and
every vocabulary is a versioned, checkable file.

Status: v0.3, experimental. Formats and APIs may change before 1.0.

## What works in v0.3

* A Rust workspace with a generic tokenizer interface and a registry, so new
  algorithms and adapters plug in without changes to the core.
* Algorithms: `bpe` (byte-level BPE, native Rust), `sequence-bpe` (BPE over
  named symbols such as events), and the baselines `bytes`, `char` and
  `dictionary`. `rustbpe` is available as a reference backend, built
  separately from `backends/rustbpe`.
* Adapters: `binary`, `text` (both byte-exact), `json`, `jsonl` (canonicalizing)
  and `jsonl-events` (keeps one field per record).
* KVF 0.1, the vocabulary file format, with content digests, embedded test
  vectors and strict validation. See `specifications/kvf/KVF-0.1.md`.
* A CLI (`kute`), Python bindings, export to tiktoken's format, and a
  reproducible benchmark suite.
* Two token containers: plain varint ids, and a compressed form
  (`kute encode --compress`) that is smaller than gzip of the original on
  the suite's large text case. See `benchmarks/README.md` for where it is not.
* Text encoding that matches tiktoken id for id on an exported vocabulary and
  runs faster than it on the same machine; long texts use every core.
* An experimental adapter contract for Holographic Resonance Medium (HRM)
  research. It is an interface only. See "What KUTE does not claim".

New in v0.3:

* `kute discover`: recurring patterns in event streams, with support,
  evidence and a held-out check, no tokenizer needed. Measured on a public
  log dataset in `benchmarks/README.md`.
* `kute encode --unknown escape`: a frozen event vocabulary carries symbols
  it has never seen and still decodes exactly; `strict` refuses them.
* `kute evolve`: the next version of an event vocabulary, with new symbols
  or sequences appended and every existing token id unchanged.
* `kute peer serve` / `kute peer send`: two processes agree on a vocabulary
  by digest, transfer it if needed, extend it together and exchange encoded
  messages with a receipt. No encryption or authentication. See
  `specifications/protocol`.
* `kute train --stream-bytes`: training memory that does not grow with
  file size (227 MiB against 959 MiB on a 500 MB input), and a peak-memory
  figure in every training report.
* Vocabulary identity specified rule by rule and checked by a second
  implementation in Python (`conformance/`), with recorded vectors that
  include files written by v0.2.0.
* `kute train --no-test-vectors` for private corpora, `kute decode
  --max-units` for untrusted sequences, and a registry that refuses a second
  registration of a name.

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
