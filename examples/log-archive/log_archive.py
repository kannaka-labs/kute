"""An operational log archive built on KUTE: exact round trip and patterns.

    pip install kute-engine
    python log_archive.py --log OpenSSH_full.log \
        --structured OpenSSH_full.log_structured.csv --work work

This is an application, not part of KUTE. It imports the published `kute`
package and the Python standard library, nothing else, and talks to no
service. It only reads the log; nothing it learns is acted on.

What it does with a log file and its Loghub "structured" companion (one row
per log line, with an EventId column naming the line's template):

 1. Splits the log by position: the first 80% of lines to learn from, the
    last 20% held out.
 2. Archive. Trains a byte-level vocabulary on the first part, encodes the
    held-out lines to token ids, packs the ids, decodes them again and
    checks that the bytes that come back are the bytes that went in.
 3. Patterns. Groups the event ids of the first part by session (the
    process id in `name[pid]:`), asks `kute.discover` for recurring
    sequences, and then counts each one again in the held-out sessions,
    which discovery never saw.

It writes `report.json` in the work directory and prints a summary.
"""
import argparse
import csv
import hashlib
import json
import lzma
import pathlib
import re
import struct
import sys
import time
import zlib

import kute

SESSION = re.compile(rb"\[(\d+)\]:")


def pack(ids, vocabulary_size):
    """Token ids as fixed-width little-endian integers."""
    width = 1 if vocabulary_size <= 1 << 8 else 2 if vocabulary_size <= 1 << 16 else 4
    return struct.pack(f"<{len(ids)}{ {1: 'B', 2: 'H', 4: 'I'}[width] }", *ids), width


def occurrences(sequence, pattern):
    n, hits = len(pattern), 0
    for i in range(len(sequence) - n + 1):
        if sequence[i:i + n] == pattern:
            hits += 1
    return hits


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--log", required=True, type=pathlib.Path)
    ap.add_argument("--structured", required=True, type=pathlib.Path)
    ap.add_argument("--work", required=True, type=pathlib.Path)
    ap.add_argument("--vocab-size", type=int, default=8192)
    ap.add_argument("--held-out", type=float, default=0.2, help="fraction of lines, from the end")
    ap.add_argument("--top", type=int, default=20, help="patterns to check on the held-out sessions")
    args = ap.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)

    lines = args.log.read_bytes().splitlines(keepends=True)
    with args.structured.open(newline="", encoding="utf-8") as f:
        events = [row["EventId"] for row in csv.DictReader(f)]
    if len(events) != len(lines):
        raise SystemExit(f"{len(lines)} log lines but {len(events)} structured rows")
    cut = int(len(lines) * (1 - args.held_out))
    learn, held = b"".join(lines[:cut]), b"".join(lines[cut:])
    learn_path = args.work / "learn.log"
    learn_path.write_bytes(learn)

    # --- archive -----------------------------------------------------------
    engine = kute.Engine()
    started = time.perf_counter()
    vocabulary = engine.train(learn_path, vocabulary_id="example:log-archive", vocab_size=args.vocab_size)
    train_seconds = time.perf_counter() - started
    vocabulary_path = args.work / "log.kvf"
    vocabulary.save(vocabulary_path, overwrite=True)

    started = time.perf_counter()
    ids = engine.encode(held, vocabulary)
    encode_seconds = time.perf_counter() - started
    started = time.perf_counter()
    back = engine.decode_bytes(ids, vocabulary)
    decode_seconds = time.perf_counter() - started
    exact = bytes(back) == held
    packed, width = pack(ids, len(vocabulary))
    (args.work / "held-out.ids").write_bytes(packed)

    archive = {
        "held_out_bytes": len(held),
        "tokens": len(ids),
        "bytes_per_token": round(len(held) / len(ids), 3),
        "round_trip_exact": exact,
        "packed_ids_bytes": len(packed),
        "id_width_bytes": width,
        "packed_ids_then_xz_bytes": len(lzma.compress(packed, preset=9)),
        "vocabulary_file_bytes": vocabulary_path.stat().st_size,
        "vocabulary_digest": vocabulary.digest,
        "for_comparison": {
            "xz_9_of_held_out_bytes": len(lzma.compress(held, preset=9)),
            "zlib_9_of_held_out_bytes": len(zlib.compress(held, 9)),
        },
        "seconds": {"train": round(train_seconds, 2), "encode": round(encode_seconds, 2), "decode": round(decode_seconds, 2)},
    }

    # --- patterns ----------------------------------------------------------
    def sessions(first, last):
        grouped = {}
        for i in range(first, last):
            found = SESSION.search(lines[i])
            grouped.setdefault(found.group(1) if found else b"", []).append(events[i])
        return list(grouped.values())

    learn_sessions, held_sessions = sessions(0, cut), sessions(cut, len(lines))
    started = time.perf_counter()
    found = kute.discover(learn_sessions)
    discover_seconds = time.perf_counter() - started
    contiguous = [p for p in found["patterns"] if p["kind"] == "contiguous"][: args.top]
    learn_events, held_events = cut, len(lines) - cut
    checked = []
    for p in contiguous:
        again = sum(occurrences(s, p["symbols"]) for s in held_sessions)
        checked.append({
            "symbols": p["symbols"],
            "support_learned": p["support"],
            "per_1000_events_learned": round(1000 * p["support"] / learn_events, 2),
            "support_held_out": again,
            "per_1000_events_held_out": round(1000 * again / held_events, 2),
        })
    patterns = {
        "sessions_learned": len(learn_sessions),
        "sessions_held_out": len(held_sessions),
        "event_names": found["observation"]["alphabet"],
        "patterns_reported": len(found["patterns"]),
        "checked": checked,
        "checked_seen_again": sum(1 for c in checked if c["support_held_out"] > 0),
        "checked_at_half_the_learned_rate_or_more": sum(1 for c in checked if c["per_1000_events_held_out"] >= 0.5 * c["per_1000_events_learned"]),
        "seconds": {"discover": round(discover_seconds, 2)},
    }

    report = {
        "kute_package": getattr(kute, "__version__", "unknown"),
        "log": {"file": args.log.name, "lines": len(lines), "bytes": len(learn) + len(held), "sha256": hashlib.sha256(learn + held).hexdigest()},
        "split": {"lines_learned": cut, "lines_held_out": len(lines) - cut},
        "archive": archive,
        "patterns": patterns,
    }
    (args.work / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    a = archive
    print(f"log: {len(lines):,} lines; learned from {cut:,}, held out {len(lines) - cut:,}")
    print(f"archive: {a['held_out_bytes']:,} bytes -> {a['tokens']:,} tokens ({a['bytes_per_token']} bytes per token); round trip exact: {exact}")
    print(f"  packed ids {a['packed_ids_bytes']:,} bytes, then xz {a['packed_ids_then_xz_bytes']:,}; vocabulary file {a['vocabulary_file_bytes']:,}")
    print(f"  xz -9 of the same bytes, nothing shared: {a['for_comparison']['xz_9_of_held_out_bytes']:,}")
    print(f"patterns: {patterns['patterns_reported']} reported from {patterns['sessions_learned']:,} sessions; "
          f"{patterns['checked_seen_again']} of the top {len(checked)} seen again in {patterns['sessions_held_out']:,} held-out sessions, "
          f"{patterns['checked_at_half_the_learned_rate_or_more']} at half their learned rate or more")
    for c in checked[:5]:
        print(f"  {' '.join(c['symbols'])}: {c['per_1000_events_learned']} per 1000 events learned, {c['per_1000_events_held_out']} held out")
    return 0 if exact else 1


if __name__ == "__main__":
    sys.exit(main())
