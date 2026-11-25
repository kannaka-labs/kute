"""HDFS_v1 as events on one clock, in the order the log was written.

    python benchmarks/hdfs_clock.py HDFS_v1.zip OUT.jsonl [REPORT.json]

`hdfs_traces.py` gives each block's events with times counted from the
block's own start, which is no use for anything that follows a clock.
This reads the raw `HDFS.log` in the same archive instead. Every line
becomes

    {"block":"blk_-1608999687919862906","event":"E5","t":0}

in the order of the log: `t` is whole seconds since the first line, taken
from the line's own date and time; `block` is the first block id in the
line; `event` is the template of `preprocessed/HDFS.log_templates.csv`
that the line matches.

The archive also holds each block's event sequence as its authors
derived it (`preprocessed/Event_traces.csv`). The sequences produced
here are compared with those, block by block, and the report says how
many agree. Lines with no block id or matching no template are counted
and left out.
"""
import csv
import datetime
import io
import json
import re
import sys
import zipfile


def main() -> int:
    if len(sys.argv) not in (3, 4):
        print(__doc__, file=sys.stderr)
        return 2
    archive = zipfile.ZipFile(sys.argv[1])
    with archive.open("preprocessed/HDFS.log_templates.csv") as f:
        templates = list(csv.DictReader(io.TextIOWrapper(f, encoding="utf-8")))
    # Most specific first: a template with more fixed text is tried before
    # one whose text it contains.
    compiled = []
    for row in templates:
        parts = [p for p in row["EventTemplate"].split("[*]")]
        compiled.append((sum(len(p) for p in parts), row["EventId"], re.compile(".*".join(re.escape(p) for p in parts), re.S)))
    compiled.sort(key=lambda c: -c[0])
    block_id = re.compile(r"blk_-?\d+")

    first = None
    counts = {"lines": 0, "events": 0, "no_block": 0, "no_template": 0, "time_went_back": 0}
    sequences = {}
    last = 0
    latest = 0
    worst_back = 0
    with archive.open("HDFS.log") as log, open(sys.argv[2], "w", encoding="utf-8", newline="\n") as out:
        for raw in log:
            counts["lines"] += 1
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            when = datetime.datetime.strptime(line[:13], "%y%m%d %H%M%S")
            first = first or when
            t = int((when - first).total_seconds())
            block = block_id.search(line)
            if not block:
                counts["no_block"] += 1
                continue
            for _, event, pattern in compiled:
                if pattern.search(line, 13):
                    break
            else:
                counts["no_template"] += 1
                continue
            if t < last:
                counts["time_went_back"] += 1
            worst_back = max(worst_back, latest - t)
            last = t
            latest = max(latest, t)
            counts["events"] += 1
            sequences.setdefault(block.group(), []).append(event)
            out.write('{"block":"%s","event":"%s","t":%d}\n' % (block.group(), event, t))

    agree = differ = missing = 0
    with archive.open("preprocessed/Event_traces.csv") as f:
        for row in csv.DictReader(io.TextIOWrapper(f, encoding="utf-8")):
            theirs = row["Features"].strip("[]").split(",")
            ours = sequences.get(row["BlockId"])
            if ours is None:
                missing += 1
            elif ours == theirs:
                agree += 1
            else:
                differ += 1
    report = {
        **counts,
        "blocks": len(sequences),
        "span_seconds": latest,
        "largest_step_back_seconds": worst_back,
        "against_the_archive's_own_traces": {"blocks_with_the_same_sequence": agree, "blocks_that_differ": differ, "blocks_in_theirs_not_here": missing},
    }
    text = json.dumps(report, indent=1)
    print(text)
    if len(sys.argv) == 4:
        open(sys.argv[3], "w", encoding="utf-8", newline="\n").write(text + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
