# KUTE: Kannaka Universal Tokenization Engine

KUTE trains, stores, exchanges and applies vocabularies for more than text:
bytes, structured records and event sequences go through the same engine, and
every vocabulary is a versioned, checkable file.

Status: v0.1, experimental. Formats and APIs may change before 1.0.

## What works in v0.1

* A Rust workspace with a generic tokenizer interface and a registry, so new
  algorithms and adapters plug in without changes to the core.
* Algorithms: `bpe` (byte-level BPE, native Rust), `sequence-bpe` (BPE over
  named symbols such as events), and the baselines `bytes`, `char` and
  `dictionary`. `rustbpe` is available as an optional reference backend.
* Adapters: `binary`, `text` (both byte-exact), `json`, `jsonl` (canonicalizing)
  and `jsonl-events` (keeps one field per record).
* KVF 0.1, the vocabulary file format, with content digests, embedded test
  vectors and strict validation. See `specifications/kvf/KVF-0.1.md`.
* A CLI (`kute`), Python bindings, export to tiktoken's format, and a
  reproducible benchmark suite.
* An experimental adapter contract for Holographic Resonance Medium (HRM)
  research. It is an interface only. See "What KUTE does not claim".

## Quick start

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
crates/kute-bpe         byte BPE, sequence BPE, optional rustbpe backend
crates/kute-algorithms  bytes, char, dictionary
crates/kute-adapters    binary, text, json, jsonl, jsonl-events
crates/kute-vocab       KVF read/write/validate, compare, tiktoken export
crates/kute             the engine: one entry point over all of the above
crates/kute-bench       evaluation harness and the benchmark suite
crates/kute-cli         the kute command
plugins/hrm             experimental HRM representation plugin
bindings/python         Python package (PyO3, built with maturin)
specifications/         KVF 0.1, token envelope and .kute container
docs/                   use cases, HRM contract, decision records
benchmarks/             how to run the suite, and recorded results
```

## Building and testing

```
cargo test --workspace                       # no Python needed
cargo test -p kute-bpe --features rustbpe    # needs Python; see below
cd bindings/python && maturin develop --release && pytest tests
```

The `rustbpe` feature embeds a Python interpreter, because rustbpe's only
training entry point takes a Python iterator. It needs Python at build time
and at run time. On Windows, set `PYO3_PYTHON` to the interpreter and put its
directory on `PATH`. The dependency is pinned to a git revision of rustbpe,
since the crates.io release uses an older PyO3.

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
  On small datasets the vocabulary costs more than it saves. Most suite
  datasets are synthetic and are labeled so.
* **No claims about HRM.** `plugins/hrm` maps token content to deterministic
  phase vectors so that HRM experiments have a stable interface. Whether those
  representations are useful for anything is an open research question that
  this repository does not test.
* **No semantic equivalence.** Vocabulary comparison matches tokens by
  content. It says nothing about meaning.
* **No privacy.** A vocabulary can reveal frequent strings from its training
  data, and a `.kute` file is decodable by anyone with the vocabulary.

Not in v0.1: registry service, signatures, streaming, import from other
tokenizer formats, and adapters for DNA, telemetry, mathematics and quantum
circuits.

## License

Licensed under either of Apache License 2.0 (`LICENSE-APACHE`) or the MIT
license (`LICENSE-MIT`), at your option. See `CONTRIBUTING.md` for how
contributions are licensed.
