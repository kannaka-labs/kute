# Complete event records

A token sequence of event names keeps the names and nothing else. `kute
events` keeps the whole record: which entity it belongs to, when it
happened, and every other field.

    kute events pack events.jsonl --output events.kev \n        --symbol-field event --time-field t --group-field block
    kute events unpack events.kev --output back.jsonl
    kute events inspect events.kev

Stability: **experimental**. The container format (`KUTE-EV1`) and the
commands may change. Library: the `kute-events` crate.

## What comes back

The input is JSON Lines, one object per event. `unpack` gives every record
back, in the order of the source, in canonical JSON: keys sorted, no
spaces, one record per line. That is the form the `jsonl` adapter writes.

* If the source was already in that form, the output is the source byte
  for byte, and the header says `"byte_exact": true`.
* Otherwise the output holds the same records with the same values, and
  the header says `false`. The order of keys and the spacing of the
  source are not kept.

Numbers keep their spelling as far as canonical JSON keeps it: `5` stays
`5` and `5.0` stays `5.0`. A line whose meaning canonical JSON would
change (a repeated key, a number too large or too precise to survive) is
refused with its line number, as the `jsonl` adapter refuses it.

The header carries the length and the SHA-256 of what `unpack` returns,
and `unpack` checks both before returning anything.

## What a record must have

| option | field | rule |
|---|---|---|
| `--symbol-field` | the event name | required in every record; a non-empty string |
| `--time-field` | the time | when named, required in every record; a whole number written as an integer |
| `--group-field` | the entity | when named, required in every record; a string, a number or a boolean |

Every other field is an attribute and may hold any JSON value, or be
absent from some records. A record missing a required field is an error
naming the line. Nothing is skipped, defaulted or rounded.

An entity is identified by the JSON value of its field: the number `1`
and the string `"1"` are two entities.

Events are kept in source order. They are not sorted by time, and times
need not increase.

## The container

    "KUTE-EV1\n"  length of header (4 bytes, little-endian)  header (JSON)  columns

The header lists the columns in order with their kind, their codec, their
size before compression and their size in the file. A reader refuses a
codec it does not know.

| codec | a column is | written by |
|---|---|---|
| `zstd` | one Zstandard frame, window at most 128 MiB, no checksum of its own | `pack`, by default |
| `zlib` | one zlib stream | `pack --codec zlib`, and `pack` in a build without Zstandard |

`zstd` is the smaller; `zlib` is for a container that must be readable
with nothing but a zlib library. Both unpack to the same records and the
same digest.

Zstandard is a build feature, `zstd`, on unless turned off (since
0.8.1). It is the one part of `kute-events` that compiles C. A build
without it (`default-features = false` for the library,
`cargo install kute-cli --no-default-features` for the command) is Rust
only, writes zlib columns by default, and refuses a Zstandard container
with an error that names the feature. The figures in this document are
for Zstandard columns unless a row says zlib.

| column | kind | holds |
|---|---|---|
| `@names` | json | the distinct event names, in order of first use |
| `@shapes` | json | the distinct sets of keys a record has, each a sorted list |
| `@shape` | varint | for each event, which shape |
| `@entities` | json | the distinct entities, in order of first use |
| `@entity` | varint | for each event, which entity |
| `@symbols` | varint | the event names as indexes, grouped by entity: all of the first entity's events in order, then the second's |
| `@time` | delta | the times as differences, see below |
| `f:NAME` | json or delta | the values of the attribute NAME, in source order, over the events that have it |

Kinds: `json` is canonical JSON texts joined by line breaks; `varint` is
unsigned LEB128; `delta` is the zigzag LEB128 of each value minus the one
before it, starting from zero, wrapping at 64 bits.

Times are differenced either from the previous event in the source or
from the previous event of the same entity. `pack` builds both, keeps the
smaller after compression, and records which in `time_order`. An
attribute whose every value is a plain integer is stored as `delta` when
that is smaller than its text.

Nothing in a container is learned from other data, and no vocabulary is
needed to read one.

## Limits

* `pack` holds the whole input and its columns in memory. It is not a
  streaming writer.
* A line may be at most 1 MiB; a file may use at most 4,088 different
  attribute names.
* `unpack --max-bytes` (default 4 GiB) is checked against the header
  before any column is inflated; the columns together are refused past
  twice that plus 1 MiB, by their declared sizes, and each is inflated
  only to its declared size.
* A table (names, entities, shapes) with more entries than the container
  has events is refused before it is read into memory, and so is a shape
  that names a field twice or more fields than the result could hold.
  What unpacking keeps for each event is some tens of bytes, so for a
  container of very short records memory can reach several times
  `--max-bytes`; for records of a hundred bytes or more it stays under it.
* Bytes that no record uses (after a column's compressed data, or values
  left in a column) make a container invalid.
* A container cannot be read from the middle or searched without
  unpacking it.
* The event names in a container are not tokenized with a vocabulary,
  so the motifs `kute discover` finds are not used to store them. Whether
  that would make the container smaller has not been measured.

Measured sizes on real data are in `benchmarks/README.md`.
