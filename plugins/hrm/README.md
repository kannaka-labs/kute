# kute-hrm

Experimental representation adapter mapping KUTE token content to deterministic phase vectors, for Holographic Resonance Medium research. An interface only; no claims are made about what the representations are good for.

Part of [KUTE](https://github.com/kannaka-labs/kute), the Kannaka Universal Tokenization Engine. Most users
want the `kute-engine` crate (imported as `kute`) or the `kute-cli` command;
this crate is one of its building blocks.

The `compose` module adds three standard ways to make one vector from a
sequence of tokens (sum, rotated sum, binding), each with an identity
that names everything fixing its vectors. They were measured on real
event logs and did no better than exact counting; see
`docs/representations.md` in the repository.

Experimental: formats and APIs may change before 1.0.

Licensed under MIT or Apache-2.0, at your option.
