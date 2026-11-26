# Seven use cases

Each case stands alone. All of them start from the seeded example data:

```
kute sample-data --output data
```

That data is synthetic, generated from small templates, so the token counts
you see here are better than real data will give. The commands are exercised
by `crates/kute-cli/tests/cli.rs`, those of the sixth case by
`crates/kute-cli/tests/jobs.rs`, and the `kute` commands of the seventh
by `crates/kute-cli/tests/http_tls.rs` with certificates the test makes.
The `openssl` lines of the seventh are not run by any test; they were
run by hand, once, with OpenSSL 1.1.1 on Windows.

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

## 6. A vocabulary service on one machine (0.9.4)

One process trains vocabularies when asked, another machine keeps a
checked copy of them. Nothing here has authentication or encryption:
the service is for loopback, or for a network you would trust with the
corpus.

```
kute serve --listen 127.0.0.1:8787 --jobs-dir trained --vocab-dir trained --registry
```

It takes training jobs into `trained`, holds every vocabulary that was
in `trained` when it started, and gives those by digest. From another
terminal:

```
kute job --at http://127.0.0.1:8787 --input data/words-train.txt --id example:service --vocab-size 1000 --output mine.kvf
```

The command waits for the job and writes `mine.kvf` only if the file is
a valid vocabulary with that name and version whose content has the
digest the service named. That the service trained it on the corpus it
was sent is the service's word; `kute train` on the same input gives the
digest to compare.

Being a registry too, the service gives the file by its digest as
soon as the job is done (0.10). Its own operations do
not use what it trained until it is started again: what a service
encodes with is what its operator started it with. After a
restart with the same command, `example:service` can be named to
`/v1/encode`, and a copy of everything it lists can be taken:

```
kute fetch --from http://127.0.0.1:8787 --mirror copy
kute serve --listen 127.0.0.1:8788 --vocab-dir copy --registry-only
```

Each file in `copy` is named by its digest and was checked as it
arrived. The mirror never deletes, so a file the first service stops
listing stays in the copy. `docs/http.md` has the limits of each step
and what none of them does.

## 7. The same service for other machines, over TLS (0.10)

The sixth case is for one machine. To let named callers on other
machines use it, the service and each caller get a certificate from an
authority you make, and every connection is TLS 1.3 with a certificate
on both sides. KUTE makes no certificates; these lines do, with
OpenSSL. The authority's key (`ca.key`) signs for everyone: keep it off
the service.

```
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 365 -subj "/CN=example authority" -keyout ca.key -out ca.pem

printf 'subjectAltName=DNS:service.example,IP:192.0.2.10\nextendedKeyUsage=serverAuth\n' > service.ext
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -subj "/CN=service" -keyout service.key -out service.csr
openssl x509 -req -in service.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 365 -extfile service.ext -out service.pem

printf 'extendedKeyUsage=clientAuth\n' > caller.ext
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -subj "/CN=caller" -keyout caller.key -out caller.csr
openssl x509 -req -in caller.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 365 -extfile caller.ext -out caller.pem
```

The service's certificate must carry the name or address callers will
put in the URL (`subjectAltName`); a certificate for one is refused
under the other. Both `-extfile` lines matter: without one, OpenSSL
1.1.1 writes an old-format (version 1) certificate, and that is
refused before any connection.

On the service's machine, with the caller's certificate named:

```
kute peer fingerprint --cert caller.pem
kute serve --listen 0.0.0.0:8787 --jobs-dir trained --vocab-dir trained --registry \
    --tls-cert service.pem --tls-key service.key --tls-client-ca ca.pem --allow sha256:...
```

On the caller's machine, with `caller.pem`, `caller.key` and `ca.pem`:

```
kute job --at https://service.example:8787 --input data/words-train.txt --id example:service --vocab-size 1000 --output mine.kvf \
    --tls-cert caller.pem --tls-key caller.key --tls-ca ca.pem
kute fetch --from https://service.example:8787 --mirror copy \
    --tls-cert caller.pem --tls-key caller.key --tls-ca ca.pem
```

What this gives: the network between cannot read or change what is
sent, a caller without a certificate from your authority gets no answer
at all, and one not on the `--allow` list gets `403`. What it does not:
every caller that is answered can do everything the service does, and
**whoever can reach the port can keep the service from answering**, with
no certificate, by holding connections open. Put a firewall in front of
it. `docs/http.md`, "Over TLS", has the measurements and the rest of
the limits.
