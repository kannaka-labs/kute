"""Repeat the KUTE / Hugging Face `tokenizers` comparison.

    pip install kute-engine tokenizers
    python benchmarks/hf_repeat.py --train train.txt --eval eval.txt [--runs 10] [--output report.json]

Both libraries are called from Python, in alternation, `--runs` times each,
and every run is a new process that loads one library and trains once. It
was not always so: with both libraries in one process and training
repeated, each slows the other down on Windows (measured: `tokenizers`
trains in 11 s alone and 15 s beside KUTE; KUTE's first training in a
process is faster than its later ones), so ratios from one shared process
flatter whichever library suffers less. `--shared-process` keeps the old
method for comparison with earlier reports. Times are taken inside the
child and do not include starting Python or importing the library.

Each run: train a byte-level BPE vocabulary of `--vocab-size` tokens on
TRAIN, then encode EVAL (which neither saw) as one document. `tokenizers` also encodes
EVAL as a batch of lines, the form in which it uses its threads, with
`encode_batch` and, where the installed version has it, `encode_batch_fast`
(which skips token offsets and so returns what KUTE returns: ids). Every
encode is decoded and compared with EVAL. Medians and ranges are reported;
both libraries use their default thread settings.

By default the two trainers split text differently before merging (each by
its own default), so their vocabularies differ: that compares the tools as
used. With `--matched`, `tokenizers` is given KUTE's split pattern (the
GPT-4 one, pieces isolated, then bytes), so both learn byte-level BPE over
the same pieces; token counts are then expected to agree, up to how each
breaks ties between equally frequent pairs.
"""
import argparse
import json
import os
import platform
import statistics
import subprocess
import sys
import time


def summary(xs):
    return {"median": round(statistics.median(xs), 3), "min": round(min(xs), 3), "max": round(max(xs), 3), "runs": len(xs)}


# KUTE's default for text, `kute_types::config::GPT4_PATTERN`.
GPT4 = r"""'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]++[\r\n]*|\s*[\r\n]|\s+(?!\S)|\s+"""


def run_kute(args, eval_bytes, text, engine=None):
    """One training and one encode with KUTE."""
    import kute
    engine = engine or kute.Engine()
    t = time.perf_counter()
    vocab = engine.train(source=args.train, algorithm="bpe", adapter="text", vocab_size=args.vocab_size, vocabulary_id="bench:hf:repeat")
    train = time.perf_counter() - t
    t = time.perf_counter()
    ids = engine.encode(text, vocabulary=vocab)
    encode = time.perf_counter() - t
    return {"version": kute.__version__, "train": train, "encode": encode, "tokens": len(ids), "digest": vocab.digest,
            "exact": engine.decode_bytes(ids, vocabulary=vocab) == eval_bytes}


def run_tokenizers(args, eval_bytes, text):
    """One training with `tokenizers`, then its three ways of encoding."""
    import tokenizers
    from tokenizers import Regex, Tokenizer, decoders, models, pre_tokenizers, trainers
    lines = text.splitlines(keepends=True)
    tok = Tokenizer(models.BPE())
    if args.matched:
        tok.pre_tokenizer = pre_tokenizers.Sequence([
            pre_tokenizers.Split(pattern=Regex(GPT4), behavior="isolated", invert=False),
            pre_tokenizers.ByteLevel(add_prefix_space=False, use_regex=False),
        ])
    else:
        tok.pre_tokenizer = pre_tokenizers.ByteLevel(add_prefix_space=False)
    tok.decoder = decoders.ByteLevel()
    trainer = trainers.BpeTrainer(vocab_size=args.vocab_size, initial_alphabet=pre_tokenizers.ByteLevel.alphabet(), show_progress=False)
    t = time.perf_counter()
    tok.train([args.train], trainer)
    train = time.perf_counter() - t
    t = time.perf_counter()
    ids = tok.encode(text).ids
    encode = time.perf_counter() - t
    exact = tok.decode(ids).encode("utf-8") == eval_bytes
    t = time.perf_counter()
    batch = tok.encode_batch(lines)
    encode_lines = time.perf_counter() - t
    tokens_lines = {sum(len(e.ids) for e in batch)}
    encode_lines_fast = None
    if hasattr(tok, "encode_batch_fast"):
        t = time.perf_counter()
        batch = tok.encode_batch_fast(lines)
        encode_lines_fast = time.perf_counter() - t
        tokens_lines.add(sum(len(e.ids) for e in batch))
    return {"version": tokenizers.__version__, "train": train, "encode": encode, "encode_lines": encode_lines,
            "encode_lines_fast": encode_lines_fast, "tokens": len(ids), "tokens_lines": sorted(tokens_lines), "exact": exact}


def in_child(args, library):
    """Run one library once in a new process and return what it measured."""
    command = [sys.executable, os.path.abspath(__file__), "--one", library, "--train", args.train, "--eval", args.eval, "--vocab-size", str(args.vocab_size)]
    if args.matched:
        command.append("--matched")
    done = subprocess.run(command, stdout=subprocess.PIPE, check=True)
    return json.loads(done.stdout)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--train", required=True)
    ap.add_argument("--eval", required=True)
    ap.add_argument("--vocab-size", type=int, default=32768)
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--matched", action="store_true", help="give tokenizers the same split pattern as KUTE")
    ap.add_argument("--shared-process", action="store_true", help="the earlier method: both libraries in this one process, trained repeatedly")
    ap.add_argument("--one", choices=["kute", "tokenizers"], help=argparse.SUPPRESS)
    ap.add_argument("--output")
    args = ap.parse_args()

    eval_bytes = open(args.eval, "rb").read()
    text = eval_bytes.decode("utf-8")
    if args.one:
        print(json.dumps(run_kute(args, eval_bytes, text) if args.one == "kute" else run_tokenizers(args, eval_bytes, text)))
        return 0

    engine = None
    if args.shared_process:
        import kute
        engine = kute.Engine()
    ks, hs = [], []
    for run in range(args.runs):
        k = run_kute(args, eval_bytes, text, engine) if args.shared_process else in_child(args, "kute")
        h = run_tokenizers(args, eval_bytes, text) if args.shared_process else in_child(args, "tokenizers")
        ks.append(k)
        hs.append(h)
        fast = f" fast {h['encode_lines_fast']:.2f}s" if h["encode_lines_fast"] is not None else ""
        print(f"run {run + 1}: kute train {k['train']:.1f}s encode {k['encode']:.3f}s | "
              f"tokenizers train {h['train']:.1f}s encode {h['encode']:.2f}s lines {h['encode_lines']:.2f}s{fast}", file=sys.stderr, flush=True)

    def col(rows, key):
        return [r[key] for r in rows]

    report = {
        "machine": {"platform": platform.platform(), "processor": platform.processor(), "logical_cpus": os.cpu_count(), "python": platform.python_version()},
        "method": "both libraries in one process, trained repeatedly" if args.shared_process else "one training per process, one library per process",
        "train_bytes": os.path.getsize(args.train),
        "eval_bytes": len(eval_bytes),
        "vocab_size": args.vocab_size,
        "pre_tokenizers": "matched: both split by the GPT-4 pattern" if args.matched else "each library's default",
        "kute": {"version": ks[0]["version"], "train_seconds": summary(col(ks, "train")), "encode_seconds": summary(col(ks, "encode")),
                 "eval_tokens": sorted(set(col(ks, "tokens"))), "distinct_vocabulary_digests": len(set(col(ks, "digest"))), "round_trip_exact": all(col(ks, "exact"))},
        "tokenizers": {"version": hs[0]["version"], "train_seconds": summary(col(hs, "train")), "encode_seconds_one_document": summary(col(hs, "encode")),
                       "encode_seconds_batch_of_lines": summary(col(hs, "encode_lines")),
                       "encode_seconds_batch_of_lines_fast": summary(col(hs, "encode_lines_fast")) if hs[0]["encode_lines_fast"] is not None else None,
                       "eval_tokens": sorted(set(col(hs, "tokens"))),
                       "eval_tokens_batch_of_lines": sorted({t for r in hs for t in r["tokens_lines"]}), "round_trip_exact": all(col(hs, "exact"))},
    }
    out = json.dumps(report, indent=2)
    if args.output:
        open(args.output, "w", encoding="utf-8").write(out + "\n")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
