# QueenSync

Two agents exchanging ordinary structured messages, with a vocabulary
that is trained, observed, challenged by a candidate, and negotiated
between them. It is the specification's E2, built from released commands
with no change to the engine:

    python examples/queensync/queensync.py path/to/kute WORK_DIR > report.json

Python 3.9 or later, standard library only. About two minutes.

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
   The script runs them under both, to show whether the decision was
   right.
6. Two in a hundred of those tasks contain message kinds and keys that
   no vocabulary has seen. They are carried by escape.

Messages are cut into symbols: one for each brace, comma and key, one
for each character of a value. A task counts as completed when the
bytes the worker decoded are exactly the bytes the queen sent. Every
byte between the two processes passes through a counting relay; the
connection is plain loopback so the count is the protocol alone.

## What was measured

`report-kute-0.7.0-next.json`, one run, seed fixed in the script. Sizes
are reproducible; times are not.

| | |
|---|---|
| tasks completed | 1,000 of 1,000, under either vocabulary |
| tasks with unseen message kinds | 24 of 24 completed; 72 symbols escaped |
| shadow decision | **reject**: the candidate saved 0.1% of bytes and its file costs 69,089 |
| bytes on the socket, each completed task | 701 (both directions, with handshake and receipts) |
| sending the vocabulary on first contact | 86,766 bytes on the socket for a 64,122-byte file |
| a dropped connection | sender reports failure; nothing delivered; sending the batch again delivers all 100. No resuming part way |
| time for a batch of 100 tasks | 0.3 to 1.2 s, a new process and connection each batch |
| language model tokens | not measured, and unchanged by construction: see below |

The same 1,000 tasks, payload only, one task at a time:

| carried as | bytes | of JSON |
|---|---:|---:|
| JSON | 927,198 | 100% |
| KUTE compressed container | 391,941 | 42% |
| xz -9 | 382,064 | 41% |
| zlib -9 | 305,036 | 33% |
| zlib -9 with a dictionary of the first 400 tasks | 125,466 | 14% |
| zlib -9 with a dictionary of all 700 tasks seen | 117,706 | 13% |

## What it says

**The loop works.** Training, exchange, discovery, proposal, shadow
judgement and fallback by escape all ran through the released commands,
and every task arrived exactly.

**The decision rule did its job.** The candidate had 1.1% fewer tokens
and 0.1% fewer bytes. Shadow rejected it, and the live run confirms it:
391,463 bytes against 391,941.

**For these messages, a general compressor is smaller.** Plain zlib on
each task beats the KUTE container, and zlib given a dictionary made
from the same traces the vocabulary was trained on is about a third of
its size. The settings here (vocabulary of 512, values spelled by
character) were fixed before any result was seen and were not tuned
afterwards; other settings may do better and have not been tried. As
measured, this example is not a reason to use a learned vocabulary to
shrink agent messages.

**Fewer bytes between agents is not fewer model tokens.** Each agent
reads the decoded message, which is the message that was sent. A model
reading it would be given the same text whichever vocabulary carried
it. Nothing here reduces what a language model is charged for.

## Not built

* The vocabulary is not signed. It is identified by its digest, and in
  a real deployment arrives over a connection authenticated with
  certificates on both sides (`kute peer` with TLS, tested in the
  engine). A signature that outlives the connection is the
  specification's B8.
* No resuming a batch part way after a dropped connection.
* Latency of a single message. Each batch here is a new process.
