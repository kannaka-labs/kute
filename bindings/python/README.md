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

## Build

```
pip install maturin
cd bindings/python
maturin develop --release        # into the active virtualenv
# or: maturin build --release    # wheel under target/wheels
pip install pytest tiktoken
pytest tests
```
