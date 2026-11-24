"""Frozen, evolved and retrained event vocabularies on held-out events.

    python benchmarks/evolution_matrix.py --kute target/release/kute \
        --train TRAIN.jsonl --eval EVAL.jsonl --work DIR [--output report.json]

TRAIN and EVAL hold one JSON object per line with an `event` field, in time
order. TRAIN is cut in half by lines: the first half is "early", the second
"late". Every vocabulary is then used to encode EVAL, which none of them saw.

  dictionary        one token per event name, learned from all of TRAIN
  frozen            sequence-bpe on the early half, never updated
  evolved           frozen, plus the strongest patterns `kute discover`
                    finds in the late half that it does not already hold,
                    added with `kute evolve` (ids of frozen are unchanged)
  retrained         sequence-bpe on all of TRAIN, at the size of `frozen`
                    and again at the size of `evolved`
  mined             the bare alphabet plus the strongest discovered
                    patterns from all of TRAIN, no pair merging at all

Sizes are the compressed container of EVAL. Each encode is decoded and
compared with EVAL. Unknown event names are carried by escape mode.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys


def run(kute, *args):
    done = subprocess.run([str(kute), *map(str, args)], capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise SystemExit(f"kute {' '.join(map(str, args))}\n{done.stderr[-2000:]}")
    return json.loads(done.stdout) if done.stdout.strip().startswith("{") else {}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kute", required=True, type=pathlib.Path)
    ap.add_argument("--train", required=True, type=pathlib.Path)
    ap.add_argument("--eval", required=True, type=pathlib.Path)
    ap.add_argument("--work", required=True, type=pathlib.Path)
    ap.add_argument("--size", type=int, default=256, help="tokens in the frozen vocabulary")
    ap.add_argument("--add", type=int, default=32, help="patterns added when evolving")
    ap.add_argument("--output", type=pathlib.Path)
    args = ap.parse_args()
    work, kute = args.work, args.kute
    work.mkdir(parents=True, exist_ok=True)

    lines = args.train.read_bytes().splitlines(keepends=True)
    half = len(lines) // 2
    early, late = work / "early.jsonl", work / "late.jsonl"
    early.write_bytes(b"".join(lines[:half]))
    late.write_bytes(b"".join(lines[half:]))
    eval_bytes = args.eval.read_bytes()

    def train(name, source, algorithm, size):
        out = work / f"{name}.kvfb"
        run(kute, "train", "--input", source, "--id", "kute:bench:evolution", "--algorithm", algorithm,
            "--adapter", "jsonl-events", "--vocab-size", size, "--output", out, "--force")
        return out

    def patterns(source, top):
        out = work / f"discover-{source.stem}.json"
        run(kute, "discover", "--input", source, "--top", top, "--output", out)
        found = json.loads(out.read_text(encoding="utf-8"))["discovery"]["patterns"]
        return [p["symbols"] for p in found if p["kind"] == "contiguous"]

    def evolve(name, parent, additions):
        out = work / f"{name}.kvfb"
        flags = [x for a in additions for x in ("--add", " ".join(a))]
        run(kute, "evolve", "--vocab", parent, *flags, "--new-version", "0.2.0", "--output", out, "--force")
        return out

    def held(vocab):
        doc = json.loads(subprocess.run([str(kute), "convert", "--input", str(vocab), "--output", str(work / "tmp.kvf"), "--force"],
                                        capture_output=True, text=True).stdout or "{}")
        del doc
        tokens = json.loads((work / "tmp.kvf").read_text(encoding="utf-8"))["tokens"]
        return {tuple(t["symbols"]) for t in tokens if "symbols" in t}

    rows = []

    def measure(name, vocab, note):
        packed, plain, back = work / f"{name}.packed.kute", work / f"{name}.kute", work / f"{name}.out"
        a = run(kute, "encode", "--vocab", vocab, "--input", args.eval, "--output", packed, "--compress", "--unknown", "escape")
        b = run(kute, "encode", "--vocab", vocab, "--input", args.eval, "--output", plain, "--unknown", "escape")
        run(kute, "decode", "--vocab", vocab, "--input", packed, "--output", back)
        info = run(kute, "inspect", vocab)
        summary = info.get("summary", info)
        rows.append({
            "vocabulary": name,
            "what": note,
            "vocabulary_tokens": summary.get("tokens"),
            "vocabulary_file_bytes": vocab.stat().st_size,
            "tokens": a["tokens"],
            "escaped_symbols": a.get("escaped_symbols", 0),
            "plain_bytes": b["serialized_bytes"],
            "compressed_bytes": a["serialized_bytes"],
            "round_trip_exact": back.read_bytes() == eval_bytes,
        })
        print(rows[-1], file=sys.stderr)

    size, more = args.size, args.add
    measure("dictionary", train("dictionary", args.train, "dictionary", 4096), "one token per event name, all of TRAIN")
    frozen = train("frozen", early, "sequence-bpe", size)
    measure("frozen", frozen, "sequence-bpe on the early half")

    have = held(frozen)
    fresh = [p for p in patterns(late, 400) if tuple(p) not in have][:more]
    evolved = evolve("evolved", frozen, fresh)
    measure("evolved", evolved, f"frozen plus {len(fresh)} patterns discovered in the late half")

    measure("retrained", train("retrained", args.train, "sequence-bpe", size), "sequence-bpe on all of TRAIN, size of frozen")
    evolved_tokens = rows[2]["vocabulary_tokens"]
    measure("retrained-larger", train("retrained-larger", args.train, "sequence-bpe", evolved_tokens),
            "sequence-bpe on all of TRAIN, size of evolved")

    bare = train("bare", args.train, "sequence-bpe", rows[0]["vocabulary_tokens"])
    bare_tokens = json.loads(json.dumps(run(kute, "inspect", bare))).get("summary", {}).get("tokens", 0)
    mined_patterns = patterns(args.train, 400)[: max(0, size - bare_tokens)]
    measure("mined", evolve("mined", bare, mined_patterns), f"bare alphabet plus {len(mined_patterns)} discovered patterns, no pair merging")

    report = {
        "train": {"path": args.train.name, "lines": len(lines), "sha256": hashlib.sha256(args.train.read_bytes()).hexdigest()},
        "eval": {"path": args.eval.name, "bytes": len(eval_bytes), "lines": eval_bytes.count(b"\n"), "sha256": hashlib.sha256(eval_bytes).hexdigest()},
        "settings": {"frozen_size": size, "patterns_added": len(fresh)},
        "added_patterns": fresh,
        "results": rows,
    }
    text = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(text + "\n", encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
