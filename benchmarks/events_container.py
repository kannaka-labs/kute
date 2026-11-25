"""Measure `kute events pack` on complete event records against general compressors.

    python benchmarks/events_container.py KUTE_BINARY EVENTS.jsonl WORK_DIR \
        --symbol event [--time t] [--group id] [--limit N] [--label NAME]

Packs the records, unpacks them, and checks that what came back has the
digest the container declares and, when the source was canonical, is the
source byte for byte. The compressors are then run on exactly the bytes
the container gives back, so every row stores the same information.
Sizes are whole files as written. One run; times are wall-clock for the
process and include reading and writing the files.

Prints a JSON report.
"""
import argparse
import hashlib
import json
import lzma
import pathlib
import subprocess
import sys
import time
import zlib


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("kute")
    ap.add_argument("events")
    ap.add_argument("work")
    ap.add_argument("--symbol", required=True)
    ap.add_argument("--time")
    ap.add_argument("--group")
    ap.add_argument("--limit", type=int, help="use only the first N lines")
    ap.add_argument("--label")
    a = ap.parse_args()
    work = pathlib.Path(a.work)
    work.mkdir(parents=True, exist_ok=True)

    source = pathlib.Path(a.events)
    if a.limit:
        lines = []
        with open(source, "rb") as f:
            for i, line in enumerate(f):
                if i >= a.limit:
                    break
                lines.append(line)
        source = work / "source.jsonl"
        source.write_bytes(b"".join(lines))
    source_bytes = source.read_bytes()

    container, back = work / "events.kev", work / "back.jsonl"
    kute = str(pathlib.Path(a.kute).resolve())
    command = [kute, "events", "pack", str(source), "--output", str(container), "--symbol-field", a.symbol]
    for flag, value in (("--time-field", a.time), ("--group-field", a.group)):
        if value:
            command += [flag, value]
    started = time.perf_counter()
    packed = json.loads(subprocess.run(command, check=True, capture_output=True).stdout)["report"]
    pack_seconds = time.perf_counter() - started
    started = time.perf_counter()
    subprocess.run([kute, "events", "unpack", str(container), "--output", str(back)], check=True, capture_output=True)
    unpack_seconds = time.perf_counter() - started

    canonical = back.read_bytes()
    header = packed["header"]
    digest = "sha256:" + hashlib.sha256(canonical).hexdigest()
    checks = {
        "digest_matches_header": digest == header["sha256"],
        "same_records_as_source": [json.loads(x) for x in canonical.splitlines()] == [json.loads(x) for x in source_bytes.splitlines() if x.strip()],
        "byte_identical_to_source": canonical == source_bytes,
        "header_says_byte_exact": header["byte_exact"],
    }
    if not (checks["digest_matches_header"] and checks["same_records_as_source"]) or checks["byte_identical_to_source"] != checks["header_says_byte_exact"]:
        print(json.dumps(checks), file=sys.stderr)
        return 1

    def timed(f):
        started = time.perf_counter()
        n = len(f(canonical))
        return {"bytes": n, "seconds": round(time.perf_counter() - started, 2)}

    rows = {
        "zlib -9": timed(lambda b: zlib.compress(b, 9)),
        "xz -9": timed(lambda b: lzma.compress(b, preset=9)),
    }
    # A general compressor given every advantage: the extreme preset and
    # a dictionary as large as the input, up to 256 MiB.
    dictionary = min(max(len(canonical), 1 << 20), 256 << 20)
    rows["xz -9e, dictionary to fit"] = timed(lambda b: lzma.compress(b, format=lzma.FORMAT_XZ, filters=[{"id": lzma.FILTER_LZMA2, "preset": 9 | lzma.PRESET_EXTREME, "dict_size": dictionary}]))

    # The plainest column layout: one column per field, values in source
    # order as JSON texts, no grouping and no differences, plus the set of
    # keys of each record; each column through xz -9.
    plain = {}
    for text in canonical.splitlines():
        record = json.loads(text)
        plain.setdefault("@keys", []).append(",".join(record))
        for key, value in record.items():
            plain.setdefault(key, []).append(json.dumps(value, separators=(",", ":"), ensure_ascii=False))
    started = time.perf_counter()
    plain_bytes = sum(len(lzma.compress(chr(10).join(values).encode(), preset=9)) for values in plain.values())
    rows["plain columns, xz -9 each"] = {"bytes": plain_bytes, "seconds": round(time.perf_counter() - started, 2)}
    try:
        from compression import zstd  # Python 3.14

        rows["zstd -19"] = timed(lambda b: zstd.compress(b, level=19))
    except ImportError:
        pass
    size = (work / "events.kev").stat().st_size
    for row in rows.values():
        row["container_over_this"] = round(size / row["bytes"], 3)
    report = {
        "label": a.label or pathlib.Path(a.events).name,
        "layout": header["layout"],
        "events": header["events"],
        "entities": packed["entities"],
        "event_names": packed["event_names"],
        "source_bytes": len(source_bytes),
        "canonical_bytes": len(canonical),
        "canonical_sha256": digest,
        "checks": checks,
        "container": {"bytes": size, "pack_seconds": round(pack_seconds, 2), "unpack_seconds": round(unpack_seconds, 2), "time_order": header.get("time_order"), "columns": header["columns"]},
        "compressors_on_the_canonical_bytes": rows,
        "note": "one run; the container's columns are zlib streams",
    }
    print(json.dumps(report, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
