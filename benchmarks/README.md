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

## Recorded run: v0.3.0

File: `results/suite-v0.3.0-windows-x86_64.json`, same machine, command and
enwik8 file as the v0.2.0 run below, run with `kute-ref`.

* **Sizes are unchanged on the large case and the synthetic cases.** Every
  ratio for B01-large, B03, B04 and B05 equals the v0.2.0 figure to the
  digits shown: v0.3.0 did not touch the encoders or the containers. The
  two cases built from this repository's own files moved slightly because
  the files changed (B01 compressed 2.12 against gzip 2.15; B02 3.24
  against 3.30).
* All 14 evaluations reconstructed their inputs exactly, retraining
  reproduced the same digest, and on B01 and B02 the native backend and
  rustbpe again learned identical merges and produced identical encodings.
* **The timings in this file are slow and should not be compared with the
  v0.2.0 file.** The machine was busy with other work during the run: the
  large case encoded at 14 MB/s and trained in 50 s, against 51 MB/s and
  22 s recorded for v0.2.0. To check that this is the machine and not the
  release, the v0.2.0 and v0.3.0 binaries were run alternately on the same
  10 MB evaluation file right afterwards: 10.8 and 8.6 MB/s for v0.2.0, 9.4
  and 9.5 MB/s for v0.3.0, with identical token counts. The two builds
  encode at the same speed; a clean timing run has not been made.
* 1 KB encode latency: P95 0.28 ms, median 0.24 ms. 32K vocabulary to a
  ready tokenizer: 170 ms.
* The suite itself still reports peak memory as not measured. Training
  memory is measured separately; see "Training memory with streaming".

The table and discussion below are the v0.2.0 run and still describe the
sizes correctly.

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

## Discovery on real event data (v0.3.0)

`kute discover` looks for recurring patterns in symbol streams without
training a tokenizer. This run uses a public, non-text dataset: HDFS_v1 from
Loghub (https://zenodo.org/records/8196385, `HDFS_v1.zip`, SHA-256
`04f919f2…e289b01`), 11,175,629 log events from a Hadoop cluster grouped
into 575,061 per-block sequences over 29 event types.

```
python benchmarks/hdfs_traces.py HDFS_v1.zip data
kute discover --lines --input data/hdfs-traces.txt --holdout-fraction 0.2 --control --top 25
```

The first 80% of the blocks (460,049 records, 9.4 million events) were
searched; the last 20% (115,012 records, 1.8 million events) were held out.
Report: `results/discovery-hdfs-v1.json`. Discovery took 5.6 s on this
machine; the whole command, with reading, evaluation and the control, 28 s.

| | searched part | held-out part |
|---|---|---|
| share of events inside a discovered contiguous pattern | 92.7% | 97.6% |
| same, patterns found in the shuffled control | 4.5% | 0.07% |
| contiguous patterns that recur with positive cohesion | | 23 of 25 |
| gapped pairs that recur with positive cohesion | | 20 of 25 |

The strongest pattern, `E11 E9`, occurs 1,345,545 times in 454,871 of the
460,049 searched blocks, 6.8 times as often as the two events' frequencies
predict, and 338,100 times in the held-out blocks. Every pattern in the
report lists the first three places it was seen (record and offset).

What this does and does not show:

* It shows that the method recovers order that is really in the data: the
  same patterns hold on blocks it never saw, and almost none of it survives
  when order is removed.
* The control shuffles events within each block. Blocks differ in which
  events they contain, so some co-occurrence survives the shuffle: the
  control still reports 50 weak patterns, 25 of which recur in the held-out
  part. Read the coverage row, not the pattern count.
* The held-out blocks are the end of the file, and they differ from the
  rest: 22 event types instead of 29, shorter sequences. Three of the top
  gapped pairs involve events that never occur there.
* Long runs of one event produce a family of nested patterns (`E23 E23`,
  `E23 E23 E23`, ...) that each count separately in the ranking.
* Nothing here says what the patterns mean. `E11 E9` is a regularity in a
  log, not a diagnosis.
* A unit test plants a known four-event motif and a delayed pair in random
  noise and checks that both are found with exact counts and that the
  shuffled control comes back nearly empty. That data is synthetic.

## Training memory with streaming (v0.3.0)

`kute train --stream-bytes N` reads input in pieces of about N bytes, cut
after a line feed. `train` now reports the peak resident memory of the
process (Windows and Linux). Both runs below train a 32,768-token text
vocabulary on this machine; one process per run.

| input | read as | peak memory | tokens learned |
|-------|---------|-------------|----------------|
| enwik8, 100 MB | whole file | 226 MiB | reference |
| enwik8, 100 MB | 4 MiB pieces | 226 MiB | the same 32,768 tokens, same ids |
| enwik8 repeated five times, 500 MB | whole file | 959 MiB | |
| enwik8 repeated five times, 500 MB | 4 MiB pieces | 227 MiB | |

* At 100 MB the file is not what fills memory; the chunk table and the
  merge trainer are. Streaming changes nothing there.
* At 500 MB the whole-file run grows with the file and the streamed run
  does not. The 500 MB input is the same text five times over, which keeps
  the chunk table the same size by construction: it isolates the effect of
  file size and says nothing about a corpus with five times the variety.
* The streamed vocabulary has a different digest from the whole-file one,
  since the piece size is recorded and the corpus fingerprint covers
  records. `kute compare` shows the token tables are identical.
* Run times are not reported: the machine was under other load and they
  varied by more than the difference between the two modes.
* Peak memory of encoding, of discovery and of the benchmark suite's cases
  is not measured yet.

## Pattern timing on real event data (v0.4.0)

File: `results/discovery-hdfs-v1-timed.json`. The same HDFS_v1 dataset as
above, this time as one event per line with its block and its time in whole
seconds since the block's first event (`hdfs_traces.py ... --events`,
11,175,629 lines, sha256
`9fdfebf6a58a5881a349463b426338f4323e1182d2c60ae2be7c120523c4b527`):

```sh
kute discover --input hdfs-events.jsonl --symbol-field event --group-field block \
  --time-field t --time-unit s --schema-id kute:loghub:hdfs-v1-traces \
  --holdout-fraction 0.2 --top 25
```

Grouping by block rebuilt the 575,061 sequences, and the 50 patterns found
are the same 50, with the same coverage, as in the run on the pre-grouped
file. What the times add, in seconds, searched part then held-out part:

| pattern | occurrences | p10 | median | p90 | held-out median |
|---------|------------:|----:|-------:|----:|----------------:|
| `E11 E9` | 1,345,545 | 0 | 0 | 0 | 0 |
| `E9 E11 E9 E26 E26` | 330,354 | 0 | 0 | 0 | 0 |
| `E21 E21` | 835,228 | 3 | 19 | 81 | 13 |
| `E23 E23 E23 E21` | 418,809 | 56 | 159 | 290 | 56 |
| `E23 ... E28` (gapped) | 3,733 | 887 | 924 | 956 | 2 occurrences |
| `E6 ... E18` (gapped) | 1,744 | 3,792 | 3,879 | 11,372 | 9 occurrences |
| `E25 ... E25` (gapped) | 1,467 | 3,885 | 3,903 | 11,399 | 1 occurrence |

* **Patterns that look alike as sequences differ by orders of magnitude in
  time.** Some complete within the same second more than nine times in ten;
  `E23 E23 E23 E21` takes minutes. Sequence statistics alone do not show
  this.
* **Some pairs recur at a steady interval.** Eight of the gapped pairs among
  `E6`, `E16`, `E18` and `E25` have a tenth percentile and a median both
  between 3,790 and 3,920 seconds, about 65 minutes; `E23 ... E28` sits
  between 887 and 956 seconds for the middle 80 percent. These are
  regularities in the data. What produces them was not investigated.
* **The held-out part does not confirm the long intervals.** It is the end
  of the file, where blocks are shorter: the 65-minute pairs occur between
  1 and 24 times there, too few to say anything. Where a pattern is common
  in both parts the timing has the same order of magnitude but is not equal
  (`E23 E23 E23 E21`: median 159 s against 56 s).
* Time resolution is one second, so zero means under a second.
* Quantiles come from all occurrences up to 100,000 and from a uniform
  sample of that size beyond; minimum, maximum and the count are exact.
* Reading the 644 MB event file and holding it with its timestamps peaked
  at 1.1 GiB. The whole file is held in memory; this path is not streamed.

## Against general-purpose compressors (v0.4.0)

Files: `results/compressors-enwik8.json`, `results/compressors-hdfs-v1.json`,
script `compressor_compare.py` (Zstandard 1.5.7, LZ4, xz, gzip; sizes only,
nothing timed). "With shared" adds what both sides must already hold: the
vocabulary for KUTE, the dictionary for Zstandard.

**Text: 10 MB of held-out enwik8.** KUTE is the compressed container from
the recorded run with the compact vocabulary file.

| method | bytes | ratio | with shared |
|--------|------:|------:|------------:|
| xz -9 | 2,680,608 | 3.73 | 3.73 |
| zstd -22 --long=27 | 2,745,331 | 3.64 | 3.64 |
| zstd -19 | 2,746,612 | 3.64 | 3.64 |
| zstd -19, 1 MiB dictionary trained on the 90 MB | 2,756,297 | 3.63 | 2.63 |
| zstd -19, 110 KiB trained dictionary | 3,068,943 | 3.26 | 3.14 |
| **KUTE**, 32,768-token vocabulary (155 KB) | 3,084,101 | 3.24 | 3.09 |
| zstd -3 | 3,489,545 | 2.87 | 2.87 |
| gzip -9 | 3,590,266 | 2.79 | 2.79 |
| lz4, level 12 | 4,124,097 | 2.43 | 2.43 |

**On text KUTE is not competitive with a strong compressor.** It is ahead of
gzip and of Zstandard at its default level, level with Zstandard using a
small trained dictionary, and 11 to 13 percent larger than `zstd -19` and
`xz -9`, which need nothing shared. Its container entropy-codes ids with
zlib; that is the weaker half.

**Events: HDFS_v1, the last 20 percent of blocks (115,012 blocks, 1,766,436
events).** The vocabulary is `sequence-bpe` with 256 tokens trained on the
first 80 percent, with a marker event at the end of each block so that block
boundaries are kept. Decoding the container gives back the evaluation file
exactly. The compressors were given the same events as text, one block per
line (`E5 E22 E5 ...`, 6,374,580 bytes), and ratios are against that.

| method | bytes | ratio | with shared |
|--------|------:|------:|------------:|
| **KUTE**, 256-token vocabulary (1,827 bytes) | 80,789 | 78.9 | 77.2 |
| zstd -22 --long=27 | 116,007 | 55.0 | 55.0 |
| zstd -19 | 127,914 | 49.8 | 49.8 |
| gzip -9 | 145,348 | 43.9 | 43.9 |
| zstd -19, 6.5 KB trained dictionary | 153,423 | 41.5 | 39.9 |
| xz -9 | 159,296 | 40.0 | 40.0 |
| lz4, level 12 | 177,820 | 35.8 | 35.8 |
| zstd -3 | 362,173 | 17.6 | 17.6 |

* **On this event data the KUTE container is the smallest, 30 percent under
  the best general compressor on the text form.**
* The text form flatters KUTE, so the compressors were also given a form
  that costs them nothing to parse: one byte per event and a zero byte per
  block end (1,881,448 bytes; `--event-bytes`). Then `zstd -19` reaches
  95,401 bytes, `xz -9` 106,896 and `gzip -9` 114,295. KUTE's 80,789 is
  still 15 percent smaller than the best of them, and 29 percent smaller
  than gzip, which uses the same entropy coder as the container: that gap
  is what the learned sequences contribute.
* A larger vocabulary was worse: 4,096 tokens gave 92,597 bytes. Fewer,
  longer tokens left the entropy coder less to work with.
* This is one dataset with 22 to 29 event types and long runs of one event.
  Nothing here says how it goes on events with many types or little
  repetition.

**The same events in binary record formats** (after v0.4.0; `--record-formats`,
same report file). One record per block. "Integers" replaces each event name
with a small number; the name table that needs is not counted.

| form | raw bytes | + zstd -19 | + xz -9 |
|------|----------:|-----------:|--------:|
| **KUTE container** | 182,020 (plain) | 80,789 (its own zlib) | |
| CBOR, integers | 1,881,602 | 98,072 | 109,564 |
| MessagePack, integers | 1,973,223 | 98,868 | 110,548 |
| Protocol Buffers, packed uint32 | 2,111,484 | 99,926 | 112,788 |
| MessagePack, names | 6,581,367 | 133,851 | 166,720 |
| CBOR, names | 6,489,746 | 133,918 | 166,176 |
| Protocol Buffers, repeated string | 8,256,108 | 145,026 | 174,628 |

* Uncompressed, the KUTE container is a tenth the size of the tightest
  binary record form: learned sequences replace runs of events with one id.
* Compressed, it is 18 percent smaller than the best of them. A strong
  compressor recovers most of what the vocabulary learned.
* The Protocol Buffers bytes are the wire format written by the script, not
  by `protoc`. Records here hold event names only; timestamps and other
  fields, which these formats are built to carry, are not in the comparison.

## Compact vocabulary file (v0.4.0)

`kute convert --input enwik8-32k.kvf --output enwik8-32k.kvfb` on the
32,768-token vocabulary from the large text case. The command reads the
output back, checks it is the same vocabulary, and times five loads of each
file from memory.

| form | bytes | against JSON |
|------|------:|-------------:|
| JSON (`.kvf`) | 3,335,566 | 1 |
| JSON through `gzip -9` | 530,969 | 6.3x smaller |
| JSON through `xz -9` | 347,684 | 9.6x smaller |
| compact (`.kvfb`) | 155,266 | 21.5x smaller |

* Same digest in both forms, and converting back gives the same JSON bytes.
* The saving comes from not storing what the merge rules already determine,
  which is why it beats general compressors on the JSON. It applies to
  learned vocabularies. A 40-token event vocabulary went from 5,433 to 978
  bytes, mostly because the JSON layout is roomy.
* **Loading is not clearly faster.** Median load of the 32K vocabulary was
  121 ms for JSON and 65 ms and 100 ms for the compact file in two runs on a
  busy machine; on the 40-token vocabulary the compact file was slightly
  slower (0.44 ms against 0.32 ms). Both forms are validated in full and
  their digest recomputed on load, and that is where the time goes.
* What it changes in the large text case: the compressed container plus the
  JSON vocabulary is 1.56 times smaller than the 10 MB evaluation text, as
  the table for the recorded run says. With the compact vocabulary in its
  place the same sum works out to 3.09, against 2.78 for gzip, which needs
  no vocabulary. That figure is arithmetic on the recorded sizes
  (9,999,811 / (3,084,101 + 155,266)); the suite still reports the JSON
  figure.

