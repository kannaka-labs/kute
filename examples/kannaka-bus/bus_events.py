"""A Kannaka consumer of KUTE: the memory events of the Kannaka bus.

    python bus_events.py --dump memory-events.jsonl --work work

Reads a dump of a NATS JetStream stream (made by bus_dump.py: one line a
message, with its sequence number, server time in nanoseconds, subject and
payload) and does two things with it, using the published `kute` package
and the Python standard library:

  1. keeps every event whole in a KUTE events container, proves that the
     records come back exactly, and compares the container's size with
     gzip, xz and Zstandard over the same bytes;
  2. looks for sequences of operations that recur within a burst of one
     agent's activity, on the earlier part of the stream, and counts how
     often they occur in the later part it had not seen.

It only reads. Nothing it finds is sent anywhere or acted on, and the
report it writes holds counts, sizes, operation names and agent names:
no payload text.
"""
import argparse
import collections
import gzip
import hashlib
import json
import lzma
import os
import statistics
import sys
import time

import kute

try:  # Python 3.14 and later
    from compression import zstd
except ImportError:  # pragma: no cover
    zstd = None

GAP_NS = 10 * 60 * 10**9  # a burst ends when an agent is silent this long
LEARN_SHARE = 0.7


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def read(dump):
    """Yield one record per bus message whose payload is a JSON object."""
    skipped = collections.Counter()
    records = []
    with open(dump, encoding="utf-8") as lines:
        for line in lines:
            message = json.loads(line)
            parts = message["subject"].split(".")
            try:
                payload = json.loads(message["data"])
            except ValueError:
                skipped["payload is not JSON"] += 1
                continue
            if not isinstance(payload, dict) or len(parts) != 5:
                skipped["not an operation event"] += 1
                continue
            record = dict(payload)
            # Where the bus put it. The payload's own fields keep their names.
            record["bus_op"] = parts[4]
            record["bus_agent"] = parts[3]
            record["bus_ns"] = message["ns"]
            record["bus_seq"] = message["seq"]
            records.append(record)
    return records, dict(skipped)


def general(data):
    """General-purpose compressors over the same bytes, each at its strongest ordinary setting."""
    ways = {"gzip_9": lambda: gzip.compress(data, 9), "xz_9_extreme": lambda: lzma.compress(data, preset=9 | lzma.PRESET_EXTREME)}
    if zstd is not None:
        ways["zstd_19"] = lambda: zstd.compress(data, level=19)
        ways["zstd_22"] = lambda: zstd.compress(data, level=22)
    sizes, seconds = {}, {}
    for name, way in ways.items():
        started = time.perf_counter()
        sizes[name] = len(way())
        seconds[name] = round(time.perf_counter() - started, 2)
    return sizes, seconds


def occurrences(sequence, pattern):
    n, k, i = 0, len(pattern), 0
    while i + k <= len(sequence):
        if sequence[i:i + k] == pattern:
            n += 1
            i += k
        else:
            i += 1
    return n


def bursts(records):
    """Each agent's operations, cut where it was silent for GAP_NS."""
    by_agent = collections.defaultdict(list)
    for r in records:
        by_agent[r["bus_agent"]].append((r["bus_ns"], f'{r["bus_op"]}/{r.get("via") or "none"}'))
    out = []
    for agent, events in by_agent.items():
        events.sort()
        current = [events[0]]
        for event in events[1:]:
            if event[0] - current[-1][0] > GAP_NS:
                out.append((agent, current))
                current = []
            current.append(event)
        out.append((agent, current))
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dump", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--min-support", type=int, default=20)
    args = parser.parse_args()
    os.makedirs(args.work, exist_ok=True)

    records, skipped = read(args.dump)
    records.sort(key=lambda r: r["bus_seq"])
    source = ("\n".join(canonical(r) for r in records) + "\n").encode("utf-8")

    # 1. The archive.
    started = time.perf_counter()
    container, packed = kute.events_pack(source, symbol_field="bus_op", time_field="bus_ns", group_field="bus_agent")
    pack_seconds = time.perf_counter() - started
    started = time.perf_counter()
    back = kute.events_unpack(container)
    unpack_seconds = time.perf_counter() - started
    exact = back == source
    with open(os.path.join(args.work, "memory-events.kev"), "wb") as f:
        f.write(container)
    sizes, seconds = general(source)
    sizes["kute_events_container"] = len(container)
    text_bytes = sum(len(r.get("content", "").encode("utf-8")) for r in records if isinstance(r.get("content"), str))

    # The same without the free text, which no column layout can help with.
    lean_records = [{k: v for k, v in r.items() if k != "content"} for r in records]
    lean = ("\n".join(canonical(r) for r in lean_records) + "\n").encode("utf-8")
    lean_container, _ = kute.events_pack(lean, symbol_field="bus_op", time_field="bus_ns", group_field="bus_agent")
    lean_exact = kute.events_unpack(lean_container) == lean
    lean_sizes, _ = general(lean)
    lean_sizes["kute_events_container"] = len(lean_container)

    # 2. Recurring sequences, learned on the earlier part and counted on the later.
    all_bursts = bursts(records)
    all_bursts.sort(key=lambda b: b[1][0][0])
    times = sorted(r["bus_ns"] for r in records)
    cut = times[int(len(times) * LEARN_SHARE)]
    learn = [b for b in all_bursts if b[1][-1][0] < cut]
    held = [b for b in all_bursts if b[1][0][0] >= cut]
    straddling = len(all_bursts) - len(learn) - len(held)
    symbols = lambda group: [[s for _, s in events] for _, events in group]  # noqa: E731
    found = kute.discover(symbols(learn), min_support=args.min_support, max_length=6)
    learn_events = sum(len(e) for _, e in learn)
    held_events = sum(len(e) for _, e in held)
    rows = []
    for pattern in found["patterns"]:
        if pattern.get("kind") != "contiguous":
            continue
        run = pattern["symbols"]
        in_learn = sum(occurrences(s, run) for s in symbols(learn))
        in_held = sum(occurrences(s, run) for s in symbols(held))
        durations = []
        agents = set()
        for agent, events in held:
            names = [s for _, s in events]
            i = 0
            while i + len(run) <= len(names):
                if names[i:i + len(run)] == run:
                    durations.append((events[i + len(run) - 1][0] - events[i][0]) / 1e9)
                    agents.add(agent)
                    i += len(run)
                else:
                    i += 1
        rows.append({
            "sequence": run,
            "occurrences_learned_on": in_learn,
            "per_1000_events_learned_on": round(1000 * in_learn / max(learn_events, 1), 2),
            "occurrences_held_out": in_held,
            "per_1000_events_held_out": round(1000 * in_held / max(held_events, 1), 2),
            "agents_held_out": len(agents),
            "median_seconds_first_to_last_held_out": round(statistics.median(durations), 3) if durations else None,
        })
    rows.sort(key=lambda r: -r["occurrences_held_out"])
    longer = [r for r in rows if len(set(r["sequence"])) > 1]

    operations = collections.Counter(f'{r["bus_op"]}/{r.get("via") or "none"}' for r in records)
    report = {
        "what": "memory operation events of the Kannaka bus (NATS JetStream), read once, read-only",
        "kute_version": kute.__version__,
        "dump_sha256": hashlib.sha256(open(args.dump, "rb").read()).hexdigest(),
        "events": len(records),
        "messages_left_out": skipped,
        "agents": len({r["bus_agent"] for r in records}),
        "days_covered": round((times[-1] - times[0]) / 86400e9, 1),
        "operations": dict(operations.most_common()),
        "archive": {
            "source": "every event as canonical JSON, one a line, in bus order",
            "source_bytes": len(source),
            "of_which_free_text_bytes": text_bytes,
            "round_trip_exact": exact,
            "byte_exact_header": packed.get("header", {}).get("byte_exact"),
            "bytes": sizes,
            "container_over_best_general_compressor": round(len(container) / min(v for k, v in sizes.items() if k != "kute_events_container"), 3),
            "general_compressor_seconds": seconds,
            "pack_seconds": round(pack_seconds, 2),
            "unpack_seconds": round(unpack_seconds, 2),
            "columns": len(packed.get("header", {}).get("columns", [])),
        },
        "archive_without_free_text": {
            "source": "the same events with the `content` field removed",
            "source_bytes": len(lean),
            "round_trip_exact": lean_exact,
            "bytes": lean_sizes,
            "container_over_best_general_compressor": round(len(lean_container) / min(v for k, v in lean_sizes.items() if k != "kute_events_container"), 3),
        },
        "sequences": {
            "symbol": "operation/via, for example remember/cli",
            "burst": "one agent's events with no silence longer than ten minutes",
            "bursts": len(all_bursts),
            "learned_on": {"bursts": len(learn), "events": learn_events, "share_of_time_ordered_events": LEARN_SHARE},
            "held_out": {"bursts": len(held), "events": held_events},
            "bursts_across_the_cut_used_for_neither": straddling,
            "min_support": args.min_support,
            "runs_found": len(rows),
            "runs_of_more_than_one_kind_of_operation": len(longer),
            "top_by_held_out_occurrences": rows[:12],
            "top_mixed_by_held_out_occurrences": longer[:12],
        },
        "limits": [
            "One stream, one read. The stream keeps what its retention allows, so this is what was there, not everything that happened.",
            "A recurring sequence is a regularity in the order of operations. Nothing here says what it means or that it causes anything.",
            "The held-out part is later in time and has a different mix of agents; a rate that differs between the two parts may be the mix.",
            "The payloads are private. The report holds no payload text, and the dump and the container stay on the machine that made them.",
        ],
    }
    with open(os.path.join(args.work, "report.json"), "w", encoding="utf-8") as f:
        json.dump(report, f, indent=1)
    json.dump(report, sys.stdout, indent=1)
    return 0 if exact and lean_exact else 1


if __name__ == "__main__":
    sys.exit(main())
