# kute (Python)

Python bindings for the Kannaka Universal Tokenization Engine. The Rust engine
does the work; this package is a thin layer over it.

```python
from kute import Engine, Vocabulary

engine = Engine()
vocab = engine.train(
    source="./agent_messages.jsonl",
    algorithm="bpe",
    adapter="jsonl",
    vocab_size=2048,
    vocabulary_id="example:agent:messages",
)
tokens = engine.encode('{"from": "kannaka", "intent": "request_observation"}', vocabulary=vocab)
original = engine.decode(tokens, vocabulary=vocab)
results = engine.evaluate(vocabulary=vocab, dataset="./evaluation.jsonl")
vocab.save("agent-vocabulary.kvf")
same = Vocabulary.load("agent-vocabulary.kvf")
```

`encode` returns a list of token ids. Those ids only mean something with the
vocabulary that produced them, so `decode` takes the same vocabulary.

`decode` returns `str` for vocabularies trained through the `text` adapter and
`bytes` for everything else. `decode_bytes` always returns `bytes`.

Since 0.5.0:

```python
small = engine.train("corpus/", vocabulary_id="example:text", vocab_size=8192)
large = engine.train("corpus/", vocabulary_id="example:text", vocab_size=32768, resume_from=small)
# large is the vocabulary one run to 32768 would give (bpe, sequence-bpe)

found = kute.discover([["START", "HEAT", "DONE"], ...], min_support=5)
found["patterns"]   # recurring runs and gapped pairs, strongest first
```

Added in 0.9:

```python
# Whole event records, by column. Every field comes back.
container, report = kute.events_pack(open("events.jsonl", "rb").read(),
                                     symbol_field="event", time_field="t", group_field="unit")
records = kute.events_unpack(container)          # canonical JSON Lines, source order
kute.events_inspect(container)                   # the header alone

# Numbers, exactly or within a bound that is checked on every value.
container, report = kute.signals_pack(values, max_error=0.01)
values_back = kute.signals_unpack(container)     # up to 16,777,216 values; max_values= for more
report["header"]["max_error_observed"]

# The next version of an event vocabulary; every existing id is kept.
child = vocabulary.evolve([["PURGE"], ["FLOW", "ALARM", "PURGE"]], version="0.2.0")
```

`kute.ZSTD` says whether the package was built with Zstandard; `codec="zlib"`
is always available. An evolved vocabulary is not better by being newer:
on the data measured so far it was rejected about as often as accepted
(`docs/shadow.md`).

`embed_test_vectors=True` stores short excerpts of the training data in the
vocabulary for `verify` to check. It is off unless asked.

## Build

```
pip install maturin
cd bindings/python
maturin develop --release        # into the active virtualenv
# or: maturin build --release    # wheel under target/wheels
pip install pytest tiktoken
pytest tests
```
