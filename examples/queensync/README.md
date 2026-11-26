# QueenSync

Two agents exchanging ordinary structured messages, with a vocabulary
that is trained, observed, challenged by a candidate, and negotiated
between them. It is the specification's E2, built from released commands
with no change to the engine:

    python examples/queensync/queensync.py path/to/kute WORK_DIR > report.json

Python 3.9 or later, standard library only. About four minutes.

**The workload is synthetic and both agents are scripted.** No language
model is called. What this example shows is that the pieces work
together and what they cost; it is not evidence about real agent
traffic.

## What happens

1. A queen assigns tasks; a worker calls tools and reports. Each task is
   about seven JSON messages.
2. `kute train` makes a vocabulary from the first 400 tasks.
3. The work changes: two procedures appear that the vocabulary never
   saw. 300 such tasks are exchanged with `kute peer`, then
   `kute discover` looks at them and `kute evolve` proposes the 32
   strongest recurring runs as tokens.
4. `kute shadow` judges the candidate on 300 further tasks.
5. If approved, `kute peer send --propose` negotiates it and the last
   1,000 tasks run under it. If rejected, they run under the old one.
   The script negotiates whatever the decision, and says in the report
   that it was forced when it was, so that the path runs every time and
   not only when the data approves. It runs the tasks under both
   vocabularies, to show whether the decision was right.
   The candidate is also offered to a worker that takes vocabularies
   only from the queen's key (`kute peer serve
   --require-signature-from`), two ways. Whole: the file unsigned,
   signed by another key, and signed by the queen (`kute sign`). And by
   proposal: the queen proposes the additions and sends with them her
   signature of the vocabulary they derive (`kute peer send --propose
   ... --sign-with`); the worker derives it and keeps it only if the
   signature holds for what it derived.
6. Two in a hundred of those tasks contain message kinds and keys that
   no vocabulary has seen. They are carried by escape.

Messages are cut into symbols: one for each brace, comma and key, one
for each character of a value. A task counts as completed when the
bytes the worker decoded are exactly the bytes the queen sent. Every
byte between the two processes passes through a counting relay; the
connection is plain loopback so the count is the protocol alone.

## What was measured

`report-kute-0.9.4.json`, one run, seed fixed in the script. Sizes
are reproducible; times are not. Apart from the signed steps, which
are new, every size in it equals the run recorded on 0.7.0. Each signed
offer carries one batch of 100 tasks, about 67 kB when delivered; a
byte or two of these figures moves between runs with the port number.

| | |
|---|---|
| tasks completed | 1,000 of 1,000, under either vocabulary |
| tasks with unseen message kinds | 24 of 24 completed; 72 symbols escaped |
| shadow decision | **reject**: the candidate saved 0.1% of bytes and its file costs 69,089 |
| bytes on the socket, each completed task | 701 (both directions, with handshake and receipts) |
| sending the vocabulary on first contact | 86,766 bytes on the socket for a 64,122-byte file |
| negotiating the candidate (forced: the decision was reject) | 2,068 bytes on the socket; the same digest on both sides |
| the candidate sent whole, unsigned, to a worker that requires the queen's signature | refused, told why; 0 of 100 tasks; 93,431 bytes on the socket before the refusal |
| sent whole, signed by a key the worker was not told of | refused, told why; 0 of 100 tasks; 93,780 bytes |
| sent whole, signed by the queen | taken; 100 of 100 tasks; 161,062 bytes with the tasks; the worker's copy verifies and has the digest of the unsigned file; the signature adds 267 bytes to the file |
| proposed to the same worker, unsigned | refused before anything is derived; 0 of 100 tasks; 2,130 bytes |
| proposed, signed by a key the worker was not told of | refused, nothing kept; 0 of 100 tasks; 2,337 bytes |
| proposed, signed by the queen | taken; 100 of 100 tasks; 69,610 bytes with the tasks; the worker's copy verifies and has the same digest |
| a dropped connection | sender reports failure; nothing delivered; sending the batch again delivers all 100. No resuming part way |
| time for a batch of 100 tasks | about 1.2 s under either vocabulary, a new process and connection each batch |
| language model tokens | not measured, and unchanged by construction: see below |

The same 1,000 tasks, payload only, one task at a time:

| carried as | bytes | of JSON | says which shared context it needs |
|---|---:|---:|---|
| JSON | 927,198 | 100% | needs none |
| KUTE container with `--compress` | 391,941 | 42% | yes |
| KUTE container | 383,043 | 41% | yes |
| zlib -9 | 305,036 | 33% | needs none |
| KUTE container, tokens only | 156,256 | 17% | no |
| zlib -9 with a dictionary of the first 400 tasks | 125,466 | 14% | no |
| zlib -9 with a dictionary of all 700 tasks seen | 117,706 | 13% | no |

Rows are only comparable where the last column agrees. A KUTE container
names its vocabulary by id, version and digest in every message, and
that header is 226,787 of its 383,043 bytes (59%). A zlib stream made
with a dictionary does not say which dictionary; both sides are assumed
to know. The like-for-like row for it is "tokens only".

Three things to know about these rows:

* `--compress` makes a message this short larger (336 bytes against 300
  on the first task): about sixty bytes of tokens do not compress and
  the wrapper costs.
* zlib takes at most the last 32 kB of a dictionary, so the rival sees
  under a tenth of the traces the vocabulary was trained on, chosen by
  position. A trained zstd dictionary would probably do better still.
  It is not in Python's standard library, which is the only reason it
  is not here.
* xz was also run (382,064) and is left out of the table: on inputs of
  900 bytes that figure is nearly all its fixed cost per stream.

## What it says

**The loop works.** Training, exchange, discovery, proposal, shadow
judgement and fallback by escape all ran through the released commands,
and every task arrived exactly. The candidate was judged and rejected,
so a deployment would not have negotiated it; the negotiation in the
table was forced to exercise the path. It worked, and it is cheap:
2,068 bytes against 86,766 for sending a whole vocabulary.

**The decision rule did its job.** The candidate had 1.1% fewer tokens
and 0.1% fewer bytes. Shadow rejected it, and the live run confirms it:
391,463 bytes against 391,941 compressed, and exactly the same without
compression (383,043 either way). Fewer tokens did not mean fewer
bytes: each new token has a two-byte id and mostly took the place of
two one-byte ids.

**For these messages, a general compressor is smaller.** Plain zlib on
each task beats the KUTE container. With the shared context agreed
outside the message on both sides, zlib with a dictionary is still
smaller than the tokens alone, by a fifth (125,466 against 156,256).
The settings here (vocabulary of 512, values spelled by character) were
not tuned. Whole values as symbols was tried first and could not
finish, because every task id became a symbol and the alphabet
outgrew the vocabulary; the lexing was changed for that reason, before
any size had been seen (`docs/decisions.md`, ADR-0042). Other settings
may do better and have not been tried. As measured, this example is
not a reason to use a learned vocabulary to shrink agent messages.

**Fewer bytes between agents is not fewer model tokens.** Each agent
reads the decoded message, which is the message that was sent. A model
reading it would be given the same text whichever vocabulary carried
it. Nothing here reduces what a language model is charged for.

## Not built

* A signed vocabulary can now reach a worker either whole or by
  proposal, and the proposal is the cheap one: about 2 kB beside the
  tasks, against about 94 kB for the file, and a refusal costs 2 kB
  instead of 93. Whole is still needed for a worker that does not hold
  the parent, and there a refusal still comes after the file has been
  sent: a signature cannot be checked on content not yet received.
  There is no registry, expiry or revocation of keys
  (`docs/signing.md`), and the connection here is plain loopback, where
  a deployment would use certificates on both sides (`kute peer` with
  TLS, tested in the engine).
* A signature says who vouches for a file. The candidate signed here is
  the one the shadow comparison rejected.
* No resuming a batch part way after a dropped connection.
* Latency of a single message. Each batch here is a new process.
