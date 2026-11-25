# kute-signals

Numeric sequences for [KUTE](https://github.com/kannaka-labs/kute): a
signal of `f64` values kept exactly, or within an absolute error bound
that is checked on every value before the container is written.

Parts are compressed with Zstandard, which compiles C, or with zlib.
`default-features = false` gives a crate that is Rust only, writes zlib
and refuses Zstandard containers.

Experimental. See `docs/signals.md` in the repository for the contract,
the format and measurements.
