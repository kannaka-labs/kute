# kute-engine

The Kannaka Universal Tokenization Engine: train, store and apply
vocabularies for text, bytes, structured records and event sequences. The
library is imported as `kute`.

```toml
[dependencies]
kute = { package = "kute-engine", version = "0.2" }
```

```rust
use kute::{AdapterConfig, Engine, Pretokenizer, TrainingConfig, VocabularyId};

let engine = Engine::new();
let mut config = TrainingConfig::new(VocabularyId::parse("example:words")?, 1000);
config.pretokenizer = Pretokenizer::Gpt4;
let corpus = vec![std::fs::read("corpus.txt")?];
let vocabulary = engine.train("bpe", "text", &AdapterConfig::default(), &corpus, &config)?;

let tokenizer = engine.tokenizer(vocabulary)?;
let tokens = engine.encode_raw(tokenizer.as_ref(), b"some text")?;
assert_eq!(engine.decode_raw(tokenizer.as_ref(), &tokens)?, b"some text");
```

The command line tool is `kute-cli` (`cargo install kute-cli` installs the
`kute` command) and the Python package is `kute-engine` on PyPI.

Documentation, specifications and benchmark results are in the
[repository](https://github.com/kannaka-labs/kute). Experimental: formats and APIs may change before 1.0.

Licensed under MIT or Apache-2.0, at your option.
