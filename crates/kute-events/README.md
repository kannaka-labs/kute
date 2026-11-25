# kute-events

Complete event records for KUTE. A token sequence of event names keeps the
names and nothing else. `pack` keeps the whole record (which entity, when,
and every other field), exactly, and stores it by column; `unpack` gives
every record back in the order of the source, in canonical JSON.

Experimental. The container format (`KUTE-EV1`) may change.

Columns are compressed with Zstandard, which compiles C, or with zlib.
`default-features = false` gives a crate that is Rust only, writes zlib
and refuses Zstandard containers.

Part of [KUTE](https://github.com/kannaka-labs/kute).
