# kute-discover

Finds recurring structure in streams of named symbols (events, opcodes,
states) without training a tokenizer: contiguous sequences and "A, then B
within a few steps" pairs. Every pattern comes with its support, the places
it was first seen, and a measure of how much more often it occurs than its
parts would by chance. `evaluate` checks the patterns against data that was
held out, and `shuffled` gives a control with the same symbol frequencies and
no order.

What it does not do: it does not interpret patterns or say what causes them.
A pattern here is a statistical regularity in the order of symbols.

Part of [KUTE](https://github.com/kannaka-labs/kute), the Kannaka Universal Tokenization Engine. The `kute
discover` command of `kute-cli` runs it on files.

Experimental: formats and APIs may change before 1.0.

Licensed under MIT or Apache-2.0, at your option.
