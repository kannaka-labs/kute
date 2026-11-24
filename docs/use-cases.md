# Five use cases

Each case stands alone. All of them start from the seeded example data:

```
kute sample-data --output data
```

That data is synthetic, generated from small templates, so the token counts
you see here are better than real data will give. The commands are exercised
by `crates/kute-cli/tests/cli.rs`.

## 1. A text vocabulary

Train a byte-level BPE vocabulary on text, encode a held-out file, and get the
same bytes back.

```
kute train --input data/words-train.txt --id example:words --vocab-size 2000 --output words.kvf
kute encode --vocab words.kvf --input data/words-eval.txt --output words-eval.kute
kute decode --vocab words.kvf --input words-eval.kute --output words-eval.out
kute validate words.kvf
kute inspect words.kvf --longest 3
kute inspect words.kvf --token 300
kute benchmark --vocab words.kvf --dataset data/words-eval.txt
```

The `text` adapter is the default and uses the GPT-4 split pattern unless you
pass `--pretokenizer none` or `--pretokenizer regex:<pattern>`. `validate`
checks structure, the content digest and the test vectors stored in the file.
`benchmark` reports tokens, serialized size with and without the vocabulary,
gzip on the same bytes, and whether every document came back exactly.

## 2. Arbitrary binary data

Any byte string is valid input to the `binary` adapter, including bytes that
are not text.

```
kute train --input data/frames-train.bin --id example:frames --adapter binary --vocab-size 600 --output frames.kvf
kute encode --vocab frames.kvf --input data/frames-eval.bin --output frames.kute
kute decode --vocab frames.kvf --input frames.kute --output frames.out
```

`frames.out` equals `data/frames-eval.bin` byte for byte. The same vocabulary
will encode and exactly decode a file it has never seen the like of, such as
an executable; it just will not shorten it much.

## 3. Structured records: agent messages

The `jsonl` adapter parses each line and re-serializes it in canonical form
(sorted keys, no extra whitespace), so that the same record always produces
the same tokens however it was formatted.

```
kute train --input data/messages-train.jsonl --id example:agent:messages --adapter jsonl --vocab-size 1200 --output messages.kvf
kute encode --vocab messages.kvf --input data/messages-eval.jsonl --output messages.kute
kute decode --vocab messages.kvf --input messages.kute --output messages.out
kute benchmark --vocab messages.kvf --dataset data/messages-eval.jsonl
```

`messages.out` holds the same records as the input, but not the same bytes:
this adapter declares `lossless: false`, and the benchmark shows
`round_trip_canonical_exact: true` with `round_trip_raw_exact: false`. If you
need the original bytes, use the `text` adapter instead.

## 4. Event sequences: industrial alarms

Here a token is not a piece of text but a run of named events. The
`jsonl-events` adapter takes one field from each record, and `sequence-bpe`
learns which events tend to follow each other.

```
kute train --input data/alarms-train.jsonl --id kute:industrial:events \
    --algorithm sequence-bpe --adapter jsonl-events --adapter-option field=tag \
    --vocab-size 64 --output industrial.kvf
kute inspect industrial.kvf --longest 5
kute encode --vocab industrial.kvf --input data/alarms-eval.jsonl
```

`inspect` shows learned tokens such as
`["CURRENT_HI", "MOTOR_TRIP", "OPERATOR_ACK", "RESET_CMD"]`. The sample
generator builds its log from five such episodes, so finding them here shows
the mechanism works; it is not evidence about real plants.

Only the `tag` field survives this adapter. Timestamps and unit names are
dropped, and an event name the vocabulary has never seen is an error, not a
guess.

To see what the learned sequences add over plain event names, train the
`dictionary` baseline on the same data and compare:

```
kute train --input data/alarms-train.jsonl --id kute:industrial:event-names \
    --algorithm dictionary --adapter jsonl-events --adapter-option field=tag \
    --vocab-size 64 --output names.kvf
kute compare industrial.kvf names.kvf
```

## 5. Using a KUTE vocabulary from tiktoken

A `bpe` vocabulary can be exported to tiktoken's file format.

```
kute train --input data/words-train.txt --id example:export --vocab-size 1000 --reserved "<|endoftext|>" --output export.kvf
kute export --vocab export.kvf --format tiktoken --output export.tiktoken
```

This writes `export.tiktoken` and `export.tiktoken.json` (split pattern,
special tokens, vocabulary size). In Python:

```python
import json, tiktoken
from tiktoken.load import load_tiktoken_bpe

meta = json.load(open("export.tiktoken.json"))
enc = tiktoken.Encoding(
    name=meta["name"],
    pat_str=meta["pat_str"],
    mergeable_ranks=load_tiktoken_bpe("export.tiktoken"),
    special_tokens=meta["special_tokens"],
    explicit_n_vocab=meta["explicit_n_vocab"],
)
```

`bindings/python/tests/test_kute.py` checks that tiktoken, loaded this way,
produces the same ids as KUTE on a set of inputs including non-Latin text and
unseen words. Export is refused for vocabularies tiktoken cannot represent
(symbol vocabularies, or two tokens with the same bytes).
