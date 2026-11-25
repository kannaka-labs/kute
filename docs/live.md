# Live discovery

`kute live` finds recurring sequences in events as they arrive, by the
time the events carry, in windows that forget, with memory fixed by its
options.

    kute live events.jsonl --symbol-field event --time-field t --group-field block --pane 3600
    some-source | kute live --symbol-field event --time-field t --pane 60 --panes 5 --lateness 10

Stability: **experimental**. Library: `kute_discover::watch`.

## In and out

Input is JSON Lines, one object per event, from a file or from standard
input. Three fields are read; the rest of each record is ignored.

| option | field | rule |
|---|---|---|
| `--symbol-field` | the event name | a string |
| `--time-field` | the event time | a whole number, in any unit; `--pane` and `--lateness` are in the same unit |
| `--group-field` | the entity | optional; a string, a number or a boolean. `1` and `"1"` are two entities |

A line that is not such an event stops the run with its line number.
With `--skip-bad-lines` it is counted and passed over.

Output is JSON Lines on standard output: `{"window": ...}` each time a
window closes, and `{"summary": ...}` when the input ends.

A window report carries the patterns (symbols, support, the most the
support can be, cohesion, evidence), the window's `start` and `end`,
what was approximated or dropped in it, the bytes of state held and the
bound on them, and two lists comparing it with the report before:
`appeared` (content hashes of patterns new in this window) and
`vanished` (patterns that were in the last report and are not in this
one).

## Time

Events are counted in order of their time, not of their arrival.

* Arrivals wait in a buffer until the **watermark** passes them. The
  watermark is the latest time seen so far minus `--lateness`.
* Events with the same time are counted in the order they arrived. The
  same arrivals always give the same reports.
* An event that arrives with a time before the watermark is **late**. It
  is dropped, and counted in `late_dropped`.
* The buffer holds at most `--max-buffered` events. Past that, the
  earliest is counted at once and the watermark jumps to its time, which
  can make later arrivals late. Counted in `forced_releases`.

With `--lateness 0` nothing waits: every event is counted on arrival and
any event older than the newest seen is late.

## Windows

Time is cut into panes of `--pane` units, aligned to zero. A window is
the last `--panes` panes: with 1 (the default) windows follow one
another without overlap; with more, the window slides forward a pane at
a time.

* A report is written when a pane that received events closes, which is
  when the first event of a later pane is counted. So a report appears
  `--lateness` after the pane's end in event time, and the last pane is
  reported when the input ends, marked `"at_end": true`.
* Stretches of time with no events produce no reports.
* Each pane has its own counts. A pane older than the window is
  discarded whole; that is how old data is forgotten.
* A sequence is counted in the pane where its last event falls. An
  entity's recent events are kept across panes, so a sequence that
  crosses a pane boundary is counted once, in the later pane.

## Bounds

Memory is a function of the options and not of the stream.
`state_bytes_bound` in every report is that function's value; it is an
estimate from the sizes of the tables, not a measurement of the process.

| option | default | when it is reached |
|---|---:|---|
| `--max-symbols` | 65,536 | further event names are folded into one; sequences through it are not counted (`events_folded`) |
| `--max-candidates` | 200,000 | per length, per pane. Counts become a Misra-Gries summary: never above the truth, under by at most `max_undercount` |
| `--max-entities` | 100,000 | the least recently seen eighth lose their recent events; a sequence spanning that is missed (`entities_evicted`) |
| `--max-buffered` | 100,000 | forced releases, above |
| `--idle` | none | not a limit reached but a rule: see below |
| `--max-length` | 6 | longest sequence counted (2 to 16) |
| `--panes` | 1 | at most 64 |

A report says `"exact": true` only if none of these was reached in its
window and nothing was dropped late or released early. `support_at_most`
covers the undercount of the summary only; what folding and eviction
lose is counted but not bounded.

`--end-on EVENT` (repeatable) names an event that ends its entity: once
it is counted, the entity is forgotten and its place freed, in time
order and not on arrival. Where entities have an end that the stream
states (a job finishing, a block deleted, a session closing), this
keeps finished entities from holding places that long-lived ones then
lose. The event must be one that only ever ends an entity: one that
also occurs part way through cuts real sequences in two.

`--idle TICKS` ends an entity's sequence by silence instead: when an
entity's last counted event is more than that long before its next, the
next starts a new sequence and the two are not joined. Entities silent
that long are also the first to go when places run out, and that is
not an eviction: by the rule, nothing would have been joined to them
anyway. It suits streams whose entity ids are reused (process ids) or
whose entities stop without saying so. `idle_ended` counts the
sequences ended this way; ones still held when the stream ends are not
counted. It cannot help when more entities are active inside the limit
than there are places. The time is the event's own, so a checkpoint
with a limit carries each entity's last time.

Measured on the OpenSSH log (638,947 events, 32,420 process ids, one
pane, 16,384 places, a lateness of 90,000 s to cover one line the log
has a day out of order):

| | evicted | sequences reported |
|---|---:|---:|
| neither | 94,254 | 150 |
| `--end-on E29` (the session-closing line) | 57,372 | 202 |
| `--idle 600` | 0 | 55 |
| both | 0 | 55 |

With the limit, 16,384 places report exactly what 1,000,000 places
report. The limit's size hardly matters on this log: of the 124,750
gaps over a minute between events of one process id, 103,078 are over a
day, which is the id being used again; 60, 600 and 3,600 s all leave no
evictions. The 95 sequences reported only without the limit were not
examined one by one, so this shows what the limit removes and not that
each removed one was false.

Figures given here before (evictions 38,931 falling to 8,196) were
measured on a file whose times were wrong: the log runs from December
into January with no year, the converter dated the second half a year
before the first, and with no lateness allowed that half, 347,943
events, was dropped as late. `benchmarks/openssh_events.py` now starts
a new year when the month falls back.

The entity limit matters most. An entity evicted between two of its
events loses the sequence across the gap, so the limit should be above
the number of entities active at once. On a replayed day of HDFS, with
213,231 blocks and the default limit of 100,000, 5% of the patterns of
an exact run were missed and some supports were far under; see
`benchmarks/README.md`.

## Stopping and going on

`--checkpoint FILE` saves the state each time `--checkpoint-every`
windows have been reported (default 1), by writing beside the file and
renaming over it. `--resume` starts from that file: it passes over the
lines of input the checkpoint had read and continues. The options must
be the same as before, and the input must be the same input from its
beginning.

* A resumed run gives the same windows as an uninterrupted one from the
  checkpoint on. A window written after the last checkpoint and before
  the stop is written again: delivery of reports is at least once, and
  `start` identifies a repeat.
* Resuming passes over lines by count. It does not check that the input
  is the one the checkpoint was made from: a file that was rotated or
  rewritten is skipped into all the same. On standard input it works
  only if the source replays the stream from its beginning.
* Stopping is killing the process; whatever checkpoint was last renamed
  into place stands. There is no separate cancel.
* Reading is synchronous. A source on a pipe is slowed to the speed of
  counting; nothing queues except the bounded buffer above.

## Not here

Durations of patterns (the offline `kute discover --time-field` reports
those), gapped pairs, windows by number of events, timers that close a
window when no event arrives, exactly-once delivery of reports, and
transports other than a file or a pipe (no NATS or MQTT adapter).
