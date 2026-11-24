# Benchmarks

## Running

```
cargo build --release -p kute-cli
./target/release/kute benchmark --suite --repo-root . --iterations 5 --output benchmarks/results/my-run.json
```

Add `--large-text path/to/enwik8` to include the large real-data case. The
recorded run used enwik8, the first 100 MB of an English Wikipedia dump, from
https://mattmahoney.net/dc/enwik8.zip. Run the suite with
`kute-ref` (built in `backends/rustbpe`, needs Python) in place of `kute` to
include the comparison with the rustbpe reference backend. To measure your own vocabulary
on your own data:

```
kute benchmark --vocab my.kvf --dataset path/to/held-out-data
```

## How to read a report

* Each case trains on one dataset and evaluates on another. Both fingerprints
  are in the report.
* `ratio_serialized` is raw bytes divided by the size of the plain `.kute`
  containers (header and varint ids). `ratio_compressed` is the same for the
  compressed containers (format 2). The `with_vocabulary` forms also count
  the vocabulary file. `ratio_gzip` is gzip at its default level on the same
  bytes. Above 1 means smaller than the input.
* `round_trip_canonical_exact` means decoding reproduced what the adapter
  produced. `round_trip_raw_exact` means it reproduced the original bytes.
  Canonicalizing adapters pass the first only, by design.
* Token counts, sizes and fingerprints are deterministic and reproduce at the
  same commit. Timings are from one machine and vary.
* Cases marked `synthetic` come from seeded template generators. They show the
  machinery works. They do not predict results on real data.

## Recorded run: v0.2.0

File: `results/suite-v0.2.0-windows-x86_64.json`. Windows 11, 8 logical CPUs
(Intel Family 6 Model 126), rustc 1.96.0, release build, 5 iterations. The
large text was enwik8 (sha256 `2b49720ec4d78c3c9fabaee6e4179a5e997302b3a70029f30f2d582218c024a8`).

Size columns are raw bytes divided by output bytes; above 1 is smaller than
the input.

| case | data | vocabulary | fewer tokens than baseline | plain container | compressed container | compressed, with vocabulary | gzip | exact |
|------|------|-----------:|---------------------------:|----------------:|---------------------:|----------------------------:|-----:|-------|
| B01-large natural language | enwik8: 90 MB train, 10 MB eval | 32,768 | 3.92x | 2.03 | **3.24** | 1.56 | 2.78 | bytes |
| B01 natural language | this repo's docs, 9 KB eval | 2,048 | 3.24x | 1.71 | 2.06 | 0.05 | 2.10 | bytes |
| B02 source code | this repo's Rust, 114 KB eval | 4,096 | 3.33x | 1.79 | 3.20 | 0.27 | 3.27 | bytes |
| B03 binary records | synthetic, 64 KB eval | 1,024 | 2.36x | 1.56 | 4.34 | 0.59 | 4.61 | bytes |
| B03 control, random bytes | synthetic, 64 KB eval | 1,024 | 1.01x | 0.67 | 0.77 | 0.39 | 1.00 | bytes |
| B04 industrial alarms | synthetic, 227 KB eval | 64 | 3.17x | 144 (tags only) | 195 (tags only) | 22.3 | 10.99 | canonical |
| B05 agent messages | synthetic, 172 KB eval | 2,048 | 30.1x | 16.8 | 23.2 | 0.70 | 15.44 | canonical |

What this run shows:

* Every evaluation document was reconstructed exactly in all 14 evaluations
  (seven cases, trained vocabulary and baseline).
* **On real text at scale, the compressed container is smaller than gzip.**
  On 10 MB of held-out Wikipedia text it is 3.24 times smaller than the
  input against 2.78 for gzip, when the reader already has the vocabulary.
  Shipping the 3.3 MB vocabulary with it brings that to 1.56. The vocabulary
  is paid for once, so the gap closes as more text is sent.
* **It does not beat gzip everywhere.** It is just behind gzip on this
  repository's small prose and source sets (2.06 against 2.10, and 3.20
  against 3.27), and further behind on synthetic binary records (4.34
  against 4.61). Stronger compressors than gzip were
  not part of this run.
* On small datasets the vocabulary still costs more than it saves: with the
  vocabulary counted, the output is larger than the input in five of the
  seven cases.
* The random-byte control behaves as it should: no token reduction, and
  both containers are larger than the input.
* B04's ratios are not compression of the log. The adapter keeps only the
  event tag and drops timestamps and unit names, so they compare a tag stream
  with full records. The like-for-like figure is 3.17x fewer tokens than one
  token per event, on a generator whose episodes are known in advance.
* B05's 30x reflects template-built messages. Real agent traffic will be far
  less repetitive.

Speed, same machine:

* Encoding the 10 MB Wikipedia set took a median of 0.20 s, 51 MB/s, on 8
  threads. Training the 32,768-token vocabulary on 90 MB took 22 s. The
  machine was under background load for the recorded run; an earlier run of
  the same code on a quieter machine gave 64 MB/s and 14 s.
* Against tiktoken 0.14.0 on the same exported vocabulary, both called from
  Python, runs alternated (`benchmarks/tiktoken_compare.py`): on one thread
  KUTE encoded at 15.9 MB/s and tiktoken at 9.1 MB/s. With all 8 threads KUTE
  reached 50 MB/s; tiktoken encodes a single document on one thread. The two
  produced identical ids for all 2,551,171 tokens.
* Timings on this machine moved by up to a factor of two between sessions.
  Treat them as indicative. The small cases run on one thread and are timed
  over a few milliseconds, so their MB/s figures in the report swing widely
  from run to run.

Engineering targets from the requirements, on this machine only (no reference
hardware is defined yet):

| target | required | measured |
|--------|----------|----------|
| lossless reconstruction | 100% | 14 of 14 evaluations exact |
| 1 KB encode latency | P95 under 5 ms | P95 0.39 ms, median 0.25 ms |
| 32K vocabulary load | under 500 ms | 121 ms to a ready tokenizer (3.3 MiB file) |
| vocabulary lookup | P95 under 1 ms | mean 4.5 ns (a mean, not a P95) |
| determinism | identical output | retraining gave the same digest |
| memory, service availability | reported | not measured |

rustbpe comparison (run with `kute-ref`, Python 3.14): on B01 and B02 the
native backend and rustbpe learned identical merge tables and produced
identical encodings of the evaluation sets. The large case was not run
through rustbpe.

Not run: B06 DNA, B07 telemetry, B08 mathematics, B09 quantum circuits and
B10 HRM states. They need adapters and datasets that do not exist yet.

## What changed since the v0.1.0 run

The v0.1.0 run (`results/suite-v0.1.0-windows-x86_64.json`) had no large
real dataset, no compressed container, and encoded text at 2 to 3 MB/s.

* The pre-tokenizer preset moved to a faster regex engine and long texts are
  encoded in parallel. Output is identical, id for id: the 10 MB evaluation
  set encoded to the same bytes before and after the change.
* The compressed container (format 2) was added.
* The large text case was added, so there is now one measurement where the
  dataset is big enough for a 32K vocabulary to make sense.
