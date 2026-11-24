# Contributing to KUTE

Thank you for looking. KUTE is early; small, well-tested changes are the
easiest to accept.

## Before you start

* For anything larger than a bug fix, open an issue first and describe what
  you want to change and why.
* Changes to file formats (`specifications/`) or to the public traits in
  `kute-core` need a decision record in `docs/decisions.md`.

## Setting up

```
cargo build --workspace
cargo test --workspace
```

Rust 1.85 or newer. No Python is needed for the default workspace. The
rustbpe backend and the Python bindings need Python 3.9 or newer; see the
README.

## What a change needs

1. **Tests.** New algorithms and adapters need round-trip tests, including
   empty input and input they must refuse. Bug fixes need a test that fails
   without the fix.
2. **No warnings.** CI builds with `-D warnings` and runs clippy.
3. **Honest capability declarations.** If an adapter or algorithm cannot
   reproduce its input exactly, it must say `lossless: false` and the docs
   must say what is lost.
4. **Safe handling of bad input.** Anything read from a file or the network is
   untrusted: check sizes and ids before allocating or indexing, and return an
   error. Library code must not panic on bad input.
5. **Determinism.** Training the same corpus with the same configuration must
   produce the same vocabulary digest. Do not iterate hash maps where order
   reaches the output, and do not write timestamps by default.

## Adding an algorithm or adapter

Implement `Tokenizer` or `ModalityAdapter` from `kute-core` and register it
with a `Registry`. Nothing in the core needs to change; third-party crates can
do the same. `crates/kute-algorithms/src/table.rs` is the smallest example of
an algorithm, `crates/kute-adapters/src/lib.rs` of an adapter.

## Benchmarks and claims

Performance or compression claims in a pull request need the benchmark report
that supports them (`kute benchmark`), with the environment section intact.
Report full serialized size, state whether the vocabulary is included, and
label synthetic data. Results that show no improvement are welcome.

## Research material

Work on HRM representations belongs in `plugins/`. Keep demonstrated results
and hypotheses visibly apart, and do not describe an untested idea as a
capability.

## Licensing of contributions

Unless you state otherwise, any contribution you submit for inclusion in KUTE
is licensed as the project is: MIT or Apache-2.0, at the user's option, with
no additional terms.

## Conduct

Be respectful and specific. Criticize code and ideas, not people.

## Security

Please report vulnerabilities privately to the maintainers rather than in a
public issue.
