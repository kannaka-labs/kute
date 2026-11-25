"""Frozen, evolved and retrained event vocabularies on data that drifts.

    python benchmarks/evolution_drift.py --kute target/release/kute --work DIR \
        [--seeds 10] [--output report.json]

The data is SYNTHETIC, made here from a seed. It exists to answer one
question that the HDFS run could not: what does a frozen vocabulary cost,
and what does evolving it recover, when the process that makes the events
changes? Nothing here says how much real data drifts.

A "regime" is a set of procedures, each a fixed sequence of three to seven
event names, drawn with unequal weights; between procedures there are
single noise events. Regime B keeps some of regime A's procedures, retires
the rest, and adds new ones that use new event names beside old ones.

  drifting    TRAIN is regime A then regime B, half each; EVAL is regime B
  stationary  TRAIN and EVAL are all regime A (the control)

For each seed and each case, `evolution_matrix.py` is run unchanged: it
cuts TRAIN in half by lines (so "early" is exactly regime A in the
drifting case), trains the frozen vocabulary on the early half, evolves it
with patterns discovered in the late half, retrains on all of TRAIN, and
encodes EVAL, which no vocabulary saw, with each. This script reports the
mean, least and greatest over the seeds, and each vocabulary's bytes
relative to the frozen one in the same run.
"""
import argparse
import json
import pathlib
import random
import statistics
import subprocess
import sys


def regime_a(rng):
    names = [f"A{i:02d}" for i in range(40)]
    procedures = [[rng.choice(names) for _ in range(rng.randint(3, 7))] for _ in range(25)]
    return names, procedures


def regime_b(rng, a_names, a_procedures):
    new = [f"B{i:02d}" for i in range(15)]
    kept = rng.sample(a_procedures, 12)
    added = []
    for _ in range(18):
        added.append([rng.choice(new) if rng.random() < 0.6 else rng.choice(a_names) for _ in range(rng.randint(3, 7))])
    return a_names + new, kept + added


def emit(rng, names, procedures, events):
    weights = [1.0 / (rank + 1) for rank in range(len(procedures))]
    out = []
    while len(out) < events:
        if rng.random() < 0.15:
            out.append(rng.choice(names))
        else:
            out.extend(rng.choices(procedures, weights)[0])
    return out[:events]


def write(path, events):
    # Bytes, so that the line ending is "\n" on every platform.
    path.write_bytes("".join(json.dumps({"event": e}, separators=(",", ":")) + "\n" for e in events).encode("utf-8"))


def make(seed, drifting, work, events):
    rng = random.Random(seed)
    a_names, a_procedures = regime_a(rng)
    b_names, b_procedures = regime_b(rng, a_names, a_procedures)
    late_names, late_procedures = (b_names, b_procedures) if drifting else (a_names, a_procedures)
    train = emit(rng, a_names, a_procedures, events) + emit(rng, late_names, late_procedures, events)
    held_out = emit(rng, late_names, late_procedures, events)
    write(work / "train.jsonl", train)
    write(work / "eval.jsonl", held_out)


def spread(values):
    return {"mean": round(statistics.mean(values), 4), "min": round(min(values), 4), "max": round(max(values), 4)}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kute", required=True, type=pathlib.Path)
    ap.add_argument("--work", required=True, type=pathlib.Path)
    ap.add_argument("--seeds", type=int, default=10)
    ap.add_argument("--events", type=int, default=100_000, help="events in each half of TRAIN and in EVAL")
    ap.add_argument("--size", type=int, default=192)
    ap.add_argument("--add", type=int, default=32)
    ap.add_argument("--output", type=pathlib.Path)
    args = ap.parse_args()
    matrix = pathlib.Path(__file__).with_name("evolution_matrix.py")

    cases = {}
    for case, drifting in (("drifting", True), ("stationary", False)):
        runs = []
        for seed in range(1, args.seeds + 1):
            work = args.work / f"{case}-{seed}"
            work.mkdir(parents=True, exist_ok=True)
            make(seed, drifting, work, args.events)
            out = work / "matrix.json"
            done = subprocess.run([sys.executable, str(matrix), "--kute", str(args.kute), "--train", str(work / "train.jsonl"), "--eval", str(work / "eval.jsonl"),
                                   "--work", str(work), "--size", str(args.size), "--add", str(args.add), "--output", str(out)], capture_output=True, text=True, encoding="utf-8")
            if done.returncode != 0:
                raise SystemExit(done.stderr[-3000:])
            rows = {r["vocabulary"]: r for r in json.loads(out.read_text(encoding="utf-8"))["results"]}
            if not all(r["round_trip_exact"] for r in rows.values()):
                raise SystemExit(f"{case} seed {seed}: a row did not decode to its input")
            runs.append(rows)
            print(f"{case} seed {seed}: frozen {rows['frozen']['compressed_bytes']} evolved {rows['evolved']['compressed_bytes']} "
                  f"retrained {rows['retrained']['compressed_bytes']} bytes; escapes in frozen {rows['frozen']['escaped_symbols']}", file=sys.stderr, flush=True)
        table = {}
        for name in runs[0]:
            table[name] = {
                "vocabulary_tokens": spread([r[name]["vocabulary_tokens"] for r in runs]),
                "tokens": spread([r[name]["tokens"] for r in runs]),
                "escaped_symbols": spread([r[name]["escaped_symbols"] for r in runs]),
                "compressed_bytes": spread([r[name]["compressed_bytes"] for r in runs]),
                "bytes_relative_to_frozen": spread([r[name]["compressed_bytes"] / r["frozen"]["compressed_bytes"] for r in runs]),
                "tokens_relative_to_frozen": spread([r[name]["tokens"] / r["frozen"]["tokens"] for r in runs]),
            }
        cases[case] = table

    report = {
        "data": "synthetic, seeded; see the script for the generator",
        "seeds": args.seeds,
        "events_per_half_of_train_and_in_eval": args.events,
        "settings": {"frozen_size": args.size, "patterns_added_at_most": args.add},
        "cases": cases,
    }
    text = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(text + "\n", encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
