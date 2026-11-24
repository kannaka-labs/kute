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
"""
import csv
import hashlib
import io
import pathlib
import sys
import zipfile


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    archive, out_dir = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "hdfs-traces.txt"
    digest = hashlib.sha256()
    records = events = 0
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
                records += 1
                events += len(symbols)
    print(f"{out_path}: {records} records, {events} events, sha256:{digest.hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
