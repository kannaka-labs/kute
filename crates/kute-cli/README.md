# kute-cli

The `kute` command for the Kannaka Universal Tokenization Engine.

```
cargo install kute-cli

kute sample-data --output data
kute train --input data/words-train.txt --id example:words --vocab-size 2000 --output words.kvf
kute encode --vocab words.kvf --input data/words-eval.txt --output words-eval.kute --compress
kute decode --vocab words.kvf --input words-eval.kute --output words-eval.out
kute validate words.kvf
```

Subcommands: `train`, `encode`, `decode`, `inspect`, `validate`, `compare`,
`export`, `benchmark`, `capabilities`, `sample-data`. Results are printed as
JSON.

Use cases, specifications and benchmark results are in the
[repository](https://github.com/kannaka-labs/kute). Experimental: formats and APIs may change before 1.0.

Licensed under MIT or Apache-2.0, at your option.
