# Experimental HRM adapter contract

Status: experimental, v0.1. An interface and one reference mapping. Nothing
here is a result.

## Purpose

Holographic Resonance Medium (HRM) experiments need tokens as complex-valued
vectors. If every experiment invents its own mapping, results cannot be
compared or repeated. This contract fixes how a mapping is named, versioned
and applied, so that two experiments that name the same mapping are using the
same inputs.

## The contract

A mapping implements `kute_core::RepresentationBackend`:

```rust
pub trait RepresentationBackend: Send + Sync {
    fn mapping_id(&self) -> &str;
    fn mapping_version(&self) -> &str;
    fn dimension(&self) -> usize;
    fn map(&self, vocabulary: &Vocabulary, token_id: u32) -> Result<Representation>;
}
```

A conforming mapping must satisfy all of these:

1. **Repeatable.** For a fixed mapping id and version, `map` returns the same
   output for the same token on any machine, in any process. No randomness
   without a recorded seed, no dependence on time or hardware.
2. **Versioned apart from the vocabulary.** Changing a mapping changes
   `mapping_version`, never the vocabulary. Token ids and KVF files stay
   valid whatever happens to a mapping.
3. **Fixed dimension.** Every output has `dimension()` components.
4. **Checked input.** An unknown token id is an error, not a default vector.
5. **Declared.** A vocabulary used with a mapping can record a reference to it
   in its `representations` section (mapping id, version, kind, optional
   location). KVF stores the reference only, never the vectors.

A mapping may be content-addressed (depends only on what the token contains)
or vocabulary-specific (depends on the id). It must document which.

## The reference mapping

`plugins/hrm` provides `HrmPhaseMapping`, id `kannaka:hrm:phase-hash`,
version `0.1.0`.

* For a token, take its content hash (see the token envelope specification).
* Hash (mapping id, mapping version, content hash, block counter) with
  SHA-256, and read 24 bits at a time as a phase in [0, 2π).
* The output is `dimension` unit phasors scaled by `1/sqrt(dimension)`, so
  every token has unit energy.

It is content-addressed: the same content gives the same vector in any
vocabulary. Two different tokens get unrelated vectors, with expected inner
product near zero and magnitude around `1/sqrt(dimension)`.

```rust
let mapping = kute_hrm::HrmPhaseMapping::new(256)?;
let vector = mapping.map(&vocabulary, token_id)?;
```

## What this does and does not show

Demonstrated, by the tests in this repository:

* the mapping is deterministic and content-addressed;
* outputs have the stated dimension and unit energy;
* invalid dimensions and unknown tokens are refused.

Not demonstrated, and not claimed:

* that these vectors carry meaning. They are hash-derived, so tokens with
  similar content do not get similar vectors;
* that an HRM fed with them retrieves, stores or generalizes better than any
  baseline;
* anything about the physical interpretation or validity of HRM dynamics.

## Hypotheses this contract is meant to let someone test

These are open questions, listed so that they are not mistaken for features:

* H1. A learned or structured mapping gives better associative recall in an
  HRM than this hash mapping, at equal dimension.
* H2. Tokens learned from event sequences are better recall units than
  single events.
* H3. Recall quality from an HRM using any such mapping beats a plain
  cosine-similarity baseline on the same data.

A test of any of them needs a stated baseline, a held-out set and repeated
runs. The hash mapping is the natural control: any structured mapping should
be compared against it.
