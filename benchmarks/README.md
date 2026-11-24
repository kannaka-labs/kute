# Benchmarks

## Running

```
cargo build --release -p kute-cli
./target/release/kute benchmark --suite --repo-root . --iterations 5 --output benchmarks/results/my-run.json
```

Add `--features rustbpe` to the build (needs Python) to include the
comparison with the rustbpe reference backend. To measure your own vocabulary
on your own data:

```
kute benchmark --vocab my.kvf --dataset path/to/held-out-data
```

## How to read a report

* Each case trains on one dataset and evaluates on another. Both fingerprints
  are in the report.
* `ratio_serialized` is raw bytes divided by the size of the `.kute`
  containers (header and varint ids). `ratio_with_vocabulary` also counts the
  vocabulary file. `ratio_gzip` is gzip at its default level on the same
  bytes. Above 1 means smaller than the input.
* `round_trip_canonical_exact` means decoding reproduced what the adapter
  produced. `round_trip_raw_exact` means it reproduced the original bytes.
  Canonicalizing adapters pass the first only, by design.
* Token counts, sizes and fingerprints are deterministic and reproduce at the
  same commit. Timings are from one machine and vary.
* Cases marked `synthetic` come from seeded template generators. They show the
  machinery works. They do not predict results on real data.

## Recorded run: v0.1.0

File: `results/suite-v0.1.0-windows-x86_64.json`. Windows 11, 8 logical CPUs
(Intel Family 6 Model 126), rustc 1.96.0, release build, 5 iterations.

| case | data | vocabulary | fewer tokens than baseline | size ratio | with vocabulary | gzip | exact |
|------|------|-----------:|---------------------------:|-----------:|----------------:|-----:|-------|
| B01 natural language | this repo's docs, 8 KB eval | 2,048 | 3.25x | 1.69 | 0.04 | 2.07 | bytes |
| B02 source code | this repo's Rust, 110 KB eval | 4,096 | 3.33x | 1.80 | 0.24 | 3.27 | bytes |
| B03 binary records | synthetic, 64 KB eval | 1,024 | 2.36x | 1.56 | 0.48 | 4.61 | bytes |
| B03 control, random bytes | synthetic, 64 KB eval | 1,024 | 1.01x | 0.67 | 0.37 | 1.00 | bytes |
| B04 industrial alarms | synthetic, 227 KB eval | 64 | 3.17x | 144 (tags only) | 21.4 | 10.99 | canonical |
| B05 agent messages | synthetic, 172 KB eval | 2,048 | 30.1x | 16.8 | 0.70 | 15.44 | canonical |

What this run shows:

* Every evaluation document was reconstructed exactly in all 12 evaluations
  (six cases, trained vocabulary and baseline).
* **KUTE token streams are not a replacement for gzip.** On the two real
  datasets and on binary records, gzip is smaller than the token container.
  Counting the vocabulary file, KUTE is larger than the raw input in five of
  six cases, because these datasets are small next to their vocabularies.
* The random-byte control behaves as it should: no token reduction, and the
  container is larger than the input.
* B04's ratio of 144 is not compression of the log. The adapter keeps only the
  event tag and drops timestamps and unit names, so it compares a tag stream
  with full records. The like-for-like figure is 3.17x fewer tokens than one
  token per event, on a generator whose episodes are known in advance.
* B05's 30x reflects template-built messages. Real agent traffic will be far
  less repetitive.
* Text encoding ran at about 2 MB/s on one thread, decoding at 60 to 420
  MB/s. Encoding speed has not been optimized.

Engineering targets from the requirements, on this machine only (no reference
hardware is defined yet):

| target | required | measured |
|--------|----------|----------|
| lossless reconstruction | 100% | 12 of 12 evaluations exact |
| 1 KB encode latency | P95 under 5 ms | P95 0.52 ms, median 0.40 ms |
| 32K vocabulary load | under 500 ms | 196 ms to a ready tokenizer (3.3 MiB file) |
| vocabulary lookup | P95 under 1 ms | mean 3.7 ns (a mean, not a P95) |
| determinism | identical output | retraining gave the same digest |
| memory, service availability | reported | not measured |

rustbpe comparison (built with the feature, Python 3.14): on B01 and B02 the
native backend and rustbpe learned identical merge tables and produced
identical encodings of the evaluation sets. Training time was about equal
(0.059 s against 0.059 s on B01; 0.142 s against 0.128 s on B02, median of
3); these corpora are too small to say which is faster.

Not run: B06 DNA, B07 telemetry, B08 mathematics, B09 quantum circuits and
B10 HRM states. They need adapters and datasets that v0.1 does not have.
