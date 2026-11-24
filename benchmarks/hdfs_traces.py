"""Turn the public HDFS_v1 log dataset into symbol records for `kute discover`.

The dataset (Loghub, https://zenodo.org/records/8196385, file HDFS_v1.zip)
holds 11 million log lines from a Hadoop cluster, already grouped into one
event sequence per storage block in preprocessed/Event_traces.csv. Each
event is a log template id such as E5. This script writes one line per
block, events separated by spaces, in the file's own order:

    python benchmarks/hdfs_traces.py HDFS_v1.zip out_dir

It writes out_dir/hdfs-traces.txt and prints the record count and the
SHA-256 of the output, so a run can be tied to its exact input. Nothing is
downloaded; fetch the archive yourself.

With --events as a third argument it also writes out_dir/hdfs-events.jsonl,
one event per line with its block and its time:

    {"block":"blk_-1608999687919862906","event":"E5","t":0}

The time is whole seconds since the block's first event, summed from the
dataset's TimeInterval column (the gaps between consecutive events).
"""
import csv
import hashlib
import io
import pathlib
import sys
import zipfile


def main() -> int:
    timed = sys.argv[3:] == ["--events"]
    if len(sys.argv) != 3 and not timed:
        print(__doc__)
        return 2
    archive, out_dir = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "hdfs-traces.txt"
    digest = hashlib.sha256()
    records = events = 0
    events_path = out_dir / "hdfs-events.jsonl"
    events_digest = hashlib.sha256()
    events_out = open(events_path, "wb") if timed else None
    with zipfile.ZipFile(archive) as z:
        name = next(n for n in z.namelist() if n.endswith("Event_traces.csv"))
        with z.open(name) as raw, open(out_path, "wb") as out:
            for row in csv.DictReader(io.TextIOWrapper(raw, encoding="utf-8", newline="")):
                symbols = [s for s in row["Features"].strip("[]").split(",") if s]
                if not symbols:
                    continue
                line = (" ".join(symbols) + "\n").encode("ascii")
                out.write(line)
                digest.update(line)
                if events_out:
                    gaps = [float(g) for g in row["TimeInterval"].strip("[]").split(",") if g.strip()]
                    assert len(gaps) == len(symbols) - 1 and all(g >= 0 and g == int(g) for g in gaps), row["BlockId"]
                    t, rows = 0, []
                    for i, symbol in enumerate(symbols):
                        if i:
                            t += int(gaps[i - 1])
                        rows.append('{"block":"%s","event":"%s","t":%d}\n' % (row["BlockId"], symbol, t))
                    chunk = "".join(rows).encode("ascii")
                    events_out.write(chunk)
                    events_digest.update(chunk)
                records += 1
                events += len(symbols)
    print(f"{out_path}: {records} records, {events} events, sha256:{digest.hexdigest()}")
    if events_out:
        events_out.close()
        print(f"{events_path}: {events} events, sha256:{events_digest.hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
