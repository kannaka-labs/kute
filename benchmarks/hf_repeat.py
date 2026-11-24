"""Repeat the KUTE / Hugging Face `tokenizers` comparison in one process.

    pip install kute-engine tokenizers
    python benchmarks/hf_repeat.py --train train.txt --eval eval.txt [--runs 10] [--output report.json]

Both libraries are called from Python, in alternation, `--runs` times each:
train a byte-level BPE vocabulary of `--vocab-size` tokens on TRAIN, then
encode EVAL (which neither saw) as one document. `tokenizers` also encodes
EVAL as a batch of lines, the form in which it uses its threads. Every
encode is decoded and compared with EVAL. Medians and ranges are reported;
both libraries use their default thread settings.

The two trainers split text differently before merging (each by its
default), so their vocabularies differ: this compares the tools as used.
"""
import argparse
import json
import os
import platform
import statistics
import sys
import time


def summary(xs):
    return {"median": round(statistics.median(xs), 3), "min": round(min(xs), 3), "max": round(max(xs), 3), "runs": len(xs)}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--train", required=True)
    ap.add_argument("--eval", required=True)
    ap.add_argument("--vocab-size", type=int, default=32768)
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--output")
    args = ap.parse_args()

    import kute
    import tokenizers
    from tokenizers import Tokenizer, decoders, models, pre_tokenizers, trainers

    eval_bytes = open(args.eval, "rb").read()
    text = eval_bytes.decode("utf-8")
    lines = text.splitlines(keepends=True)
    k = {"train": [], "encode": [], "tokens": set(), "exact": True, "digests": set()}
    h = {"train": [], "encode": [], "encode_lines": [], "tokens": set(), "tokens_lines": set(), "exact": True}
    engine = kute.Engine()

    for run in range(args.runs):
        t = time.perf_counter()
        vocab = engine.train(source=args.train, algorithm="bpe", adapter="text", vocab_size=args.vocab_size, vocabulary_id="bench:hf:repeat")
        k["train"].append(time.perf_counter() - t)
        t = time.perf_counter()
        ids = engine.encode(text, vocabulary=vocab)
        k["encode"].append(time.perf_counter() - t)
        k["tokens"].add(len(ids))
        k["digests"].add(vocab.digest)
        k["exact"] &= engine.decode_bytes(ids, vocabulary=vocab) == eval_bytes

        tok = Tokenizer(models.BPE())
        tok.pre_tokenizer = pre_tokenizers.ByteLevel(add_prefix_space=False)
        tok.decoder = decoders.ByteLevel()
        trainer = trainers.BpeTrainer(vocab_size=args.vocab_size, initial_alphabet=pre_tokenizers.ByteLevel.alphabet(), show_progress=False)
        t = time.perf_counter()
        tok.train([args.train], trainer)
        h["train"].append(time.perf_counter() - t)
        t = time.perf_counter()
        ids = tok.encode(text).ids
        h["encode"].append(time.perf_counter() - t)
        h["tokens"].add(len(ids))
        h["exact"] &= tok.decode(ids).encode("utf-8") == eval_bytes
        t = time.perf_counter()
        batch = tok.encode_batch(lines)
        h["encode_lines"].append(time.perf_counter() - t)
        h["tokens_lines"].add(sum(len(e.ids) for e in batch))
        print(f"run {run + 1}: kute train {k['train'][-1]:.1f}s encode {k['encode'][-1]:.3f}s | "
              f"tokenizers train {h['train'][-1]:.1f}s encode {h['encode'][-1]:.2f}s lines {h['encode_lines'][-1]:.2f}s", file=sys.stderr, flush=True)

    report = {
        "machine": {"platform": platform.platform(), "processor": platform.processor(), "logical_cpus": os.cpu_count(), "python": platform.python_version()},
        "train_bytes": os.path.getsize(args.train),
        "eval_bytes": len(eval_bytes),
        "vocab_size": args.vocab_size,
        "kute": {"version": kute.__version__, "train_seconds": summary(k["train"]), "encode_seconds": summary(k["encode"]),
                 "eval_tokens": sorted(k["tokens"]), "distinct_vocabulary_digests": len(k["digests"]), "round_trip_exact": k["exact"]},
        "tokenizers": {"version": tokenizers.__version__, "train_seconds": summary(h["train"]), "encode_seconds_one_document": summary(h["encode"]),
                       "encode_seconds_batch_of_lines": summary(h["encode_lines"]), "eval_tokens": sorted(h["tokens"]),
                       "eval_tokens_batch_of_lines": sorted(h["tokens_lines"]), "round_trip_exact": h["exact"]},
    }
    out = json.dumps(report, indent=2)
    if args.output:
        open(args.output, "w", encoding="utf-8").write(out + "\n")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
