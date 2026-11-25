# KUTE 0.7: what is in it, what backs it, what it does not show

This page lists the acceptance gates set for 0.7 (in
[KUTE_v0.7_Development_Specification.pdf](KUTE_v0.7_Development_Specification.pdf),
page 11) with the test or report behind each, then the limitations, then how to reproduce every number this
repository states. What changed for someone upgrading from 0.6 is in
[stability.md](stability.md).

## Gates and their evidence

The Rust tests named here run in CI on Linux and Windows on every commit.
The Python tests and `conformance/kvf_digest.py` run on Linux only.
Reports are files under `benchmarks/results` with the command that made
them in `benchmarks/README.md`.

| gate | what it asks | evidence | open remarks |
|---|---|---|---|
| AC-701 | Authenticated exchange between independent processes | `crates/kute-cli/tests/peer_tls.rs` starts two `kute` processes with certificates; an unlisted certificate is refused. `crates/kute-protocol/tests/mutual_tls.rs`. ADR-0032. | |
| AC-702 | Aggregate peer memory and vocabulary budgets under sustained load | `two_peers.rs`: `the_other_side_cannot_pass_the_aggregate_budgets`, `delivery_is_bounded_and_repeats_are_not_delivered_twice`, `a_container_is_refused_on_its_declared_size_before_it_is_inflated`, `long_symbols_are_bounded_by_their_bytes_not_only_their_count`. `tests/soak.rs`: 300,000 mixed messages against one peer, budgets checked at every step, peak memory of the process flat after warm-up. ADR-0031. | The soak is one process and seconds long, not days; it does not cover the TLS transport. |
| AC-703 | Evolution-rule negotiation rejects incompatible proposals | `two_peers.rs`: `a_proposal_under_another_evolution_rule_is_refused_before_derivation`, `an_initiator_does_not_propose_to_a_peer_with_another_rule`. | |
| AC-704 | Three materially different external datasets | enwik8 (prose), HDFS_v1 (event traces), CPython 3.12.7 `Lib` (source code); `benchmarks/README.md`, "External datasets". A fourth, an OpenSSH server log, is used by `examples/log-archive`. | The code corpus has sizes only: no timing, no other tokenizer. |
| AC-705 | Comparison under equivalent tokenizer configurations | `benchmarks/hf_repeat.py --matched`, one training per process; reports `tokenizers-enwik8-10runs-matched-isolated.json` and `...-desktop.json`. | The two machines differ (about 4 times on 24 threads, 16 times on 8). Paging and throttling were ruled out on the laptop; why its `tokenizers` time halves with process history is not known. |
| AC-706 | Persistent Python sessions with parity tests | `bindings/python/tests/test_kute.py`: `test_one_vocabulary_object_is_a_persistent_session`. ADR-0030. | The parity tested is within Python: one long-lived vocabulary object against fresh loads. Python is not compared with the Rust API in a test; both call the same engine. |
| AC-707 | MCP exposes encode, decode, discover, compare, inspect | `crates/kute-cli/tests/mcp.rs`; `docs/mcp.md`. ADR-0035. | Experimental. Standard input and output only. |
| AC-708 | Streaming discovery prototype with bounded memory | `crates/kute-discover/tests/live.rs`: state stays under its stated bound over 2 million events. ADR-0036. | A prototype. Counts are estimates; occurrences cut by eviction are not inside the reported bound. Library only. |
| AC-709 | Strict structured-data normalization | The JSON adapters refuse duplicate keys and numbers that do not survive; `untrusted_input.rs`: `damaged_structured_input_is_refused_or_canonical`. ADR-0029. | |
| AC-710 | Evolution on deliberately drifting data, with a frozen baseline | `benchmarks/evolution_drift.py`, ten seeds, with a stationary control; reports `evolution-drift-synthetic-160.json` and `evolution-drift-synthetic.json`. | Synthetic data from one generator. Evolving past 256 tokens cost bytes in half the seeds at one size. |
| AC-711 | Historical vectors pass or migrations exist | `crates/kute/tests/conformance.rs` and `conformance/kvf_digest.py` on `specifications/kvf/vectors`, including files written by v0.2.0. | v0.1.0 files are not among the vectors. |
| AC-712 | Fuzz and property tests on untrusted input | `crates/kute/tests/untrusted_input.rs`: 8,000 damaged vocabulary files, 12,000 containers, 20,000 structured inputs, seeded. | Seeded mutation, not coverage-guided fuzzing. The maintainer accepted this for 0.7 on 2026-11-24. What it leaves open: inputs that only a coverage-guided search would reach in the vocabulary, container and JSON parsers have not been tried. |
| AC-713 | API stability classes documented | [stability.md](stability.md) | |
| AC-714 | One reproducible external application | `examples/log-archive`: instructions, program, recorded report. | Reproduced by the reviewer on a second machine from the README alone, with the package from PyPI: the report was identical except for timings. |
| AC-715 | Release documentation with limitations and reproduction | This page. | |

## Limitations

What this release does not show, stated once here so that no other page
has to be read for it:

- **No claim about meaning.** Tokens and discovered patterns are
  regularities in bytes and in order. Nothing in KUTE knows what they mean,
  and no test here measures usefulness to a model or an agent.
- **Not a compressor.** On prose and on source code the KUTE container is
  10 to 13% larger than `zstd -19` and `xz` on the same bytes, before the
  vocabulary is counted. On one event dataset (HDFS event names) it was
  smaller than `zstd -19`; that is one dataset of 30 event names.
- **Speed is machine-dependent.** Under a matched configuration training
  was about 4 times faster than Hugging Face `tokenizers` on a 24-thread
  desktop and about 16 times on an 8-thread laptop. Two machines, one
  corpus, one vocabulary size; quote the range with the machines, not one
  number.
- **Evolution is not free and not always a gain.** On synthetic drifting
  data it recovered about what retraining at the same size did, and less
  than retraining at the size it grew to. Crossing 256 tokens can cost more
  bytes than it saves; `kute evolve` now warns. On real data without drift
  (HDFS) it changed nothing.
- **Streaming discovery is a prototype**, approximate by design, with no
  command-line surface.
- **The exchange protocol is not a production service.** One connection at
  a time, no registry, no revocation beyond editing the allow list, no
  resumption.
- **The HRM adapter is an interface and a reference mapping.** Nothing has
  been measured about whether it helps anything.
- **Synthetic data is labelled where it is used**: the seeded sample data,
  the benchmark suite's generated workloads, and the drift generator.
- Everything is before 1.0; see [stability.md](stability.md) for what may
  change.

## Reproducing the numbers

| claim | command | data |
|---|---|---|
| Suite results in `benchmarks/README.md` | `kute benchmark --suite --repo-root . --iterations 5` | generated, seeded; enwik8 with `--large-text` |
| Sizes against compressors, prose | `python benchmarks/compressor_compare.py` with the flags in `benchmarks/README.md` | enwik8, split 90/10 MB |
| Sizes against compressors, code | `python benchmarks/code_corpus.py`, then `kute train`, `kute encode`, `compressor_compare.py` | CPython 3.12.7 tarball, SHA-256 in `benchmarks/README.md` |
| Event sizes, discovery, timing | `python benchmarks/hdfs_traces.py`, then the commands in `benchmarks/README.md` | Loghub HDFS_v1, Zenodo 8196385 |
| Frozen, evolved, retrained | `python benchmarks/evolution_matrix.py` | HDFS_v1 |
| The same under drift | `python benchmarks/evolution_drift.py --kute target/release/kute --work DIR --size 160` | generated, seeds 1 to 10 |
| Training and encoding against `tokenizers` | `python benchmarks/hf_repeat.py --train ... --eval ... --runs 10 --matched` | enwik8 |
| Log archive | `examples/log-archive/README.md` | Loghub-2.0 OpenSSH, Zenodo 8275861 |

Reports record what ties a run to its input where the script has it: the
SHA-256 of the files read, the versions of the libraries compared against,
and for timed runs the machine. Not every report has all three. Timings
will differ on other hardware; token counts, sizes and digests should not.
