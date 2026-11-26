# What may change, and what is held

KUTE is before 1.0. Nothing here is a promise of the kind a 1.0 makes. This
page says, for each public surface, which of three classes it is in, so that
someone building on it knows what a new release may do to them.

| class | what a release may do | how you find out |
|---|---|---|
| **held** | Not change the meaning of anything already written or accepted. A break needs a new format or protocol version, a migration written down here, and the old vectors kept passing or explicitly retired. | A test in this repository fails first. |
| **provisional** | Change in a minor release (0.x to 0.y), never in a patch release. | The release notes list it. |
| **experimental** | Change or disappear in any release. | Assume it will. |

## Formats

| surface | class | notes |
|---|---|---|
| KVF vocabulary file, text form (`kvf_version` 0.1) | held | A vocabulary written by any release since v0.2.0 loads, verifies and keeps its digest. |
| Vocabulary digest (canonical JSON, SHA-256) | held | Reimplemented independently in `conformance/kvf_digest.py`, which CI runs against every recorded vocabulary. |
| Token sequence container (`.kute`), plain and compressed | held | Recorded sequences decode to the recorded bytes. |
| Token ids of a published vocabulary version | held | `kute evolve` only appends; a parent's ids keep their meaning in every child. |
| Compact vocabulary file (`.kvfb`) | provisional | Same content and digest as the text form; the byte layout may change. Keep the text form as the record. |
| Training and evolution rules (`generator` strings such as `kute evolve-3`) | provisional | A changed rule gets a new name. A vocabulary records the rule that made it, and stays valid; what may change is what a new training run produces from the same input. |
| Strict input normalization | provisional | Rejects duplicate keys and numbers that would not survive a round trip. Further rejections may be added. |
| Token envelope (`specifications/token-envelope`) | experimental | |

## Protocol

| surface | class | notes |
|---|---|---|
| Peer protocol messages (protocol number 2) | provisional | Peers that speak different numbers refuse each other in `HELLO`; there is no negotiation down. The number changes whenever a message changes. |
| Mutual TLS transport and fingerprint allow list | provisional | TLS 1.3 only. |
| Policy defaults (everything off unless asked for) | held | A release will not turn on giving vocabularies, answering queries, translating or accepting vocabularies by default. |
| Error codes | provisional | Codes may be added. |

## Command line

| surface | class | notes |
|---|---|---|
| `train`, `encode`, `decode`, `inspect`, `convert`, `export`, `evolve`, `benchmark`, `capabilities`: names and existing flags | provisional | Flags may be added. |
| JSON printed on standard output | provisional | Keys may be added; a consumer should ignore keys it does not know. |
| Exit status: 0 for success, 1 when a command fails, 2 when the command line itself is wrong (unknown command or flag, missing argument) | held | Finer exit codes do not exist yet; the error text on standard error is for people and is not a contract. |
| `discover` | provisional | The report's fields are still settling. |
| `peer serve`, `peer send`, `peer fingerprint` | provisional | Follows the protocol's class. |
| `mcp` | experimental | See `docs/mcp.md`. |
| `keygen`, `sign`, `verify`, the signature scheme, `peer serve --require-signature-from` | experimental | See `docs/signing.md`. |
| `serve --studio` | experimental | See `docs/studio.md`. |
| `serve --registry`, `serve --registry-only`, `fetch`, `GET /v1/vocabularies` | experimental | New in 0.9. See `docs/http.md`. |
| `predict`, `kute_discover::predict` | experimental | See `docs/predict.md`. |
| `shadow` | experimental | See `docs/shadow.md`. |
| `live`, `kute_discover::watch`, its checkpoint | experimental | See `docs/live.md`. |
| `events`, the `KUTE-EV1` container, the `kute-events` crate | experimental | See `docs/events.md`. |
| `signals`, the `KUTE-SG1` container, the `kute-signals` crate | experimental | See `docs/signals.md`. |
| `serve` (HTTP, `/v1/`) | experimental | See `docs/http.md`. A breaking change to the interface arrives as a new path version. |

## Libraries

| surface | class | notes |
|---|---|---|
| `kute` crate: `Engine`, `Vocabulary`, `Tokenizer`, training, encode, decode, verify | provisional | |
| `kute-types`, `kute-vocab` | provisional | |
| `kute-core`, `kute-bpe`, `kute-algorithms`, `kute-adapters` | provisional | Public so that backends and adapters can be written against them; expect more change here than in `kute`. |
| `kute-protocol` | provisional | `Policy` gains fields; build it with `..Policy::default()`. |
| `kute-discover` | provisional | |
| `kute-discover::live` | experimental | A prototype. Its counts are estimates with a stated error; see ADR-0036. |
| `kute-bench` | experimental | A tool for this repository, published so that reports can be reproduced. |
| Python package `kute-engine` (`kute.Engine`, `kute.Vocabulary`, `kute.discover`) | provisional | Tracks the Rust `kute` crate. |
| HRM adapter contract and plugin | experimental | An interface and one reference mapping. |
| `kute_hrm::compose` (bundle, permute-bundle, bind-chain) | experimental, research | Measured once, with a negative result: `docs/representations.md`. May be removed. |
| `backends/rustbpe` | experimental | A reference backend for comparison. |

Minimum supported Rust is 1.85 and is checked in CI. Raising it is a minor
release.

## Historical vectors

`specifications/kvf/vectors` holds vocabularies and sequences that are never
regenerated. The directory `v0.2.0` in it was written by the released v0.2.0
binary. On every commit:

- `crates/kute/tests/conformance.rs` loads every recorded vocabulary, runs
  its embedded test vectors, recomputes its digest and compares it with the
  recorded one; and decodes every recorded sequence and compares the result
  with the recorded bytes.
- `conformance/kvf_digest.py`, which shares no code with the Rust
  implementation, recomputes the digest of every recorded vocabulary.

All of them pass at this release, so no migration is needed for anything
written since v0.2.0. Files from v0.1.0 are not among the vectors and are
not covered.

## Changes since v0.9.2 that need action

- **Python `kute.signals_unpack` reads at most 16,777,216 values by
  default, not 134,217,728.** This is the command line's default. A
  container with more needs `max_values=`. The old default let a small
  container of compressed zeros ask a caller for several gigabytes.

## Changes since v0.9.1 that need action

None for the command line: `--status-file` and `--peer-status` are new
and off unless given. Rust: `kute-cli`'s `http::Options` has a new
field, `peer_status` (`None` is the service as it was), and
`kute_protocol::Session` gains `serve_observed`; `serve` is unchanged.
Studio has an eighth view, Peer, and its "Not here" list no longer
names peer status.

## Changes since v0.9.0 that need action

- **`kute fetch --max-bytes` defaults to 64 MiB, not 256 MiB.** A
  vocabulary file larger than that now needs the option.
- **`kute fetch` reports and errors are worded for several registries.**
  The report gains `registries_passed_over` (empty with one registry)
  and, since the same release, `signatures_in_the_file` and
  `signatures_in_the_file_means`. A failure now reads `nothing was
  written. <registry>: <why>` for each registry asked; the reason
  inside is worded as before. With one `--from`, `--list` prints what
  it did, plus `says_it_asks`; with several it prints `registries`, a
  row each.
- **The registry listing has two new fields**, `upstreams` and
  `upstream_files_need_a_trusted_signature`. A registry started without
  `--upstream` behaves as in 0.9.0.
- **Rust, `kute-cli` as a library:** `registry::fetch` and
  `registry::list` take `&[String]` where they took `&str`, and
  `http::Options` has a new field, `federation` (`None` is the service
  as it was; `..Default::default()` already gives that).

## Changes since v0.8.1 that need action

- **`kute_protocol::Message::SymbolPropose` has a new field,
  `signatures`.** Rust code that builds the variant must give it
  (`signatures: vec![]` is the message as it was), and code that matches
  it in full must name it or use `..`. On the wire nothing changes for a
  proposal without signatures, and the protocol number is still 2.
- **A peer with a vocabulary check no longer refuses every proposal.**
  `Peer::require_vocabulary_check` used to make proposals fail outright;
  now a proposal that carries signatures is derived and put to the
  check. A check that looks only at content, and not at who signed,
  will now pass a proposed vocabulary it would pass if sent whole.
  `kute peer serve --require-signature-from` may be combined with
  `--accept-proposals`; it takes proposals only when both are given.
- **The crates are on Rust edition 2024.** The minimum compiler is
  unchanged (1.85). Nothing changes for code that depends on them.
- **`kute serve` prints two more keys** when it starts (`registry`,
  `operations`), and `GET /v1/capabilities` has `registry`. Additions
  only.
- Nothing changes in a file format: vocabularies, token sequences, and
  event and signal containers written by 0.8.1 are read as before, and
  Zstandard stays the default codec.

## Changes since v0.7.0 that need action

- **`kute-events` and `kute-signals` compile C by default**, for
  Zstandard through the `zstd` crate. Since 0.8.1 that is a feature,
  `zstd`, on unless turned off: with `default-features = false` the two
  libraries are Rust only, write zlib, and refuse a Zstandard container
  by name. Corrected in 0.8.1: this note said in 0.8.0 that building
  `kute-cli` had come to need a C compiler. It already did in 0.7.0,
  through `ring` for TLS, and still does with the feature off.
- **A signal container may name its codec** (0.8.1). One written with
  zlib carries `"codec":"zlib"` in its header; 0.8.0 does not know the
  field and reports such a container as damaged. Containers written
  with Zstandard are byte for byte what 0.8.0 wrote.
- **A vocabulary file may carry signatures**, and one with more than 16,
  or with an overlong field in one, is refused. Signatures are outside
  the digest: signing changes no digest, and the engine checks none
  (`kute verify` does).
- **`kute_hrm::inner_product` sums in `f64`.** Similarities can differ
  from v0.7.0 in the last digits; the stated tolerance is 1e-4.
- **Vocabularies trained by this release have new digests**, as with
  every release: the generator string is part of the digest. The tokens
  are the same.
- Everything added in this release (`kute serve`, `kute events`,
  `kute signals`, `kute live`, `kute shadow`, `kute predict`, signing) is
  experimental: its formats (`KUTE-EV1`, `KUTE-SG1`, signature files,
  checkpoints) may change before 1.0.

## Changes since v0.6.0 that need action

- **Evolved vocabularies** made by this release record their parent by
  digest (`kute evolve-3`), not by a copy of the parent's history. Older
  evolved vocabularies stay valid and keep their digests. Anything that read
  `provenance.training.parent_training` must read `evolved_from` instead.
- **`kute peer serve` and `kute peer send`** refuse to run without TLS
  unless `--insecure-plain` is given.
- **A listening peer gives nothing by default.** Serving vocabularies,
  answering symbol queries and translating each need their flag
  (`--give-vocabularies`, `--answer-queries`, `--translate`) or `Policy`
  field.
- **The peer protocol number is 2.** A v0.6.0 peer and this one refuse each
  other.
- **Strict JSON input** rejects duplicate keys and numbers that cannot be
  represented exactly, which earlier releases accepted and rounded. The
  adapter option `strict=false` restores the earlier behaviour (ADR-0029).
- **A few JSON numbers canonicalize differently.** Decimals are now parsed
  so that they survive a round trip exactly; a number whose last digit
  earlier releases changed is now kept. Documents containing such numbers
  give different canonical bytes, and so different tokens, than under 0.6.
