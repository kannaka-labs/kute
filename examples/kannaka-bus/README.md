# A Kannaka consumer: the bus's memory events

The first use of KUTE on a Kannaka system's own data. Kannaka's agents
announce every memory operation (remember, recall, forget) on a NATS
bus, and a JetStream stream keeps them. This example reads that stream
once and does what `examples/log-archive` does for a server log: keeps
every event whole and proves it comes back, and looks for sequences of
operations that recur.

It only reads. `bus_dump.py` opens an ordered ephemeral consumer, which
the server forgets when the script ends; nothing is published, nothing on
the bus is changed, and nothing found is acted on. `bus_events.py` talks
to no service at all: it needs the published `kute` package and the
Python standard library.

## Run it

Python 3.14 for the Zstandard comparison (earlier versions skip it).

```
pip install kute-engine==0.9.1 nats-py

NATS_USER=... NATS_PASSWORD=... \
python bus_dump.py nats://HOST:4222 KANNAKA_MEMORY_EVENTS memory-events.jsonl
python bus_events.py --dump memory-events.jsonl --work work
```

`bus_dump.py` reads every subject of any JetStream stream. `bus_events.py` expects
subjects of the form `A.B.C.<agent>.<operation>` and JSON object
payloads; other messages are counted and left out. It exits 0 when both
round trips are exact and writes `work/report.json`.

## What was measured

`report-memory-events-kute-0.9.1.json`: one read of the stream on
2026-11-25, one run, on a 24-thread desktop. 125,275 events from 60
agents over 89.9 days; 4 messages were not JSON and were left out.

**The archive.** Every event as canonical JSON, one a line, is 65.5 MB,
of which 9.6 MB is the free text of memories. The container gives back
those bytes exactly.

| | bytes | seconds to make |
|---|---|---|
| KUTE events container (0.9.1, default codec) | 9,036,042 | 29 |
| xz -9 extreme | 10,356,580 | 145 |
| Zstandard -22 | 10,730,095 | 150 |
| Zstandard -19 | 10,860,618 | 54 |
| gzip -9 | 16,951,515 | under 2 |

The container is 12.8% smaller than the best of the general compressors
on the same bytes, each at its strongest ordinary setting. With the
free-text field removed from every event the figure is 13.4% (8,115,791
against 9,373,936 for xz). Unpacking takes 0.3 s. Sizes are the same on
every run; times are one run each and were not repeated, and an earlier
run of the same packing took 63 s with other work on the machine. Each
of the five ran on one thread.

What that does and does not show: on these events, laying the fields
out as columns before compressing beats compressing the lines. The
margin is much smaller than on the two public datasets in
`benchmarks/README.md`, where the container was about half the size of
tuned xz: these events are mostly hashes, identifiers and free text,
which no layout makes smaller. The comparison there with plain columns
through xz, which came within 7% of the container, was not run here. It
is one stream of one system.

**Recurring sequences.** Each event became a symbol, operation and
source (`remember/cli`, `recall/daemon`, and so on: eight kinds, and
one event whose subject named no operation, counted as `other`). Both
words come from a fixed list in the script, so nothing a payload says
can reach the report. An
agent's events were cut into bursts wherever it was silent for ten
minutes: 2,123 bursts. `kute.discover` was run on the bursts that ended
in the first 70% of events by time (1,577 bursts, 87,191 events), and
each run of adjacent operations it found was then counted in the bursts
that began after (543 bursts, 37,152 events).

It found 55 runs, 29 of them mixing more than one kind of operation.
The mixed ones do recur later, at somewhat lower rates:

| run | per 1,000 events, learned on | per 1,000 events, held out | agents, held out |
|---|---|---|---|
| remember/perception, remember/cli, remember/perception | 106.0 | 71.3 | 1 |
| recall/daemon, remember/cli, recall/daemon | 31.2 | 24.3 | 2 |
| the first of these twice over (six events) | 36.4 | 19.8 | 1 |

Read plainly: these are the loops of one or two agents that run on a
timer, seen from outside. The discovery found them without being told
they exist, and they held on data it had not seen, but each comes from
one or two agents out of sixty, so nothing here is a regularity of the
system as a whole.

The largest change between the two parts is not one of those. Runs of
`forget/cli` went from 1.7 per 1,000 events to 119.8: a mass removal of
memories happened in the later part. A rate learned on the earlier part
said nothing about it. That is the limit of this kind of counting, and
the report carries the line: a rate that differs between the two parts
may be the mix of what was running.

## Limits

* One stream, one read, one run. The stream keeps what its retention
  allows, so this is what was there on that day.
* The payloads are private: they hold the text of memories. The report
  has counts, sizes, operation names and the number of agents, and no
  payload text. The dump and the container were not published, so the
  figures above cannot be reproduced by someone without access to the
  bus. The scripts can be run on any JetStream stream of JSON events.
* A recurring run is a regularity in the order of operations. Nothing
  here says what it means or that one operation causes the next.
* Nothing in Kannaka uses this yet. It reads Kannaka's data; no Kannaka
  service packs, stores or decides anything with KUTE.
