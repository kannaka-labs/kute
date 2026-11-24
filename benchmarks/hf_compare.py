"""Byte-level BPE trained by KUTE and by Hugging Face `tokenizers`, same text.

    python benchmarks/hf_compare.py --kute target/release/kute \
        --train train.txt --eval eval.txt --work DIR [--vocab-size 32768] [--output report.json]

Both train a byte-level BPE vocabulary of the same size on TRAIN and encode
EVAL, which neither saw. Reported: wall time to train, tokens for EVAL, wall
time to encode EVAL, and whether decoding gives EVAL back byte for byte.
If `tiktoken` is installed, the token count of its pretrained `cl100k_base`
is added for scale; that vocabulary is three times larger and was trained on
other data, so it is not a like-for-like row.

The two trainers do not split text the same way before merging (each uses
its default), so their vocabularies differ; this compares the tools as they
are used, not one algorithm against itself. KUTE is timed through its CLI,
process start and file reading included, and uses every core. `tokenizers`
is timed in process, once on EVAL as a single document (one thread) and once
on EVAL as a batch of lines (its threads).
"""
import argparse
import json
import pathlib
import subprocess
import sys
import time


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kute", required=True, type=pathlib.Path)
    ap.add_argument("--train", required=True, type=pathlib.Path)
    ap.add_argument("--eval", required=True, type=pathlib.Path)
    ap.add_argument("--work", required=True, type=pathlib.Path)
    ap.add_argument("--vocab-size", type=int, default=32768)
    ap.add_argument("--output", type=pathlib.Path)
    args = ap.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)
    eval_bytes = args.eval.read_bytes()
    rows = []

    def kute(*a):
        t = time.perf_counter()
        done = subprocess.run([str(args.kute), *map(str, a)], capture_output=True, text=True, encoding="utf-8")
        if done.returncode != 0:
            raise SystemExit(done.stderr[-2000:])
        out = done.stdout.strip()
        return (json.loads(out) if out.startswith("{") else {}), time.perf_counter() - t

    vocab, packed, back = args.work / "kute.kvfb", args.work / "kute.kute", args.work / "kute.out"
    _, train_s = kute("train", "--input", args.train, "--id", "kute:bench:hf", "--algorithm", "bpe", "--adapter", "text",
                      "--vocab-size", args.vocab_size, "--output", vocab, "--force")
    enc, encode_s = kute("encode", "--vocab", vocab, "--input", args.eval, "--output", packed)
    kute("decode", "--vocab", vocab, "--input", packed, "--output", back)
    version = subprocess.run([str(args.kute), "--version"], capture_output=True, text=True).stdout.strip()
    rows.append({"tool": version, "vocabulary_tokens": args.vocab_size, "train_seconds": round(train_s, 2),
                 "eval_tokens": enc["tokens"], "encode_seconds": round(encode_s, 3),
                 "round_trip_exact": back.read_bytes() == eval_bytes})
    print(rows[-1], file=sys.stderr)

    import tokenizers
    from tokenizers import Tokenizer, decoders, models, pre_tokenizers, trainers

    tok = Tokenizer(models.BPE())
    tok.pre_tokenizer = pre_tokenizers.ByteLevel(add_prefix_space=False)
    tok.decoder = decoders.ByteLevel()
    trainer = trainers.BpeTrainer(vocab_size=args.vocab_size, initial_alphabet=pre_tokenizers.ByteLevel.alphabet(), show_progress=False)
    t = time.perf_counter()
    tok.train([str(args.train)], trainer)
    train_s = time.perf_counter() - t
    text = eval_bytes.decode("utf-8")
    t = time.perf_counter()
    ids = tok.encode(text).ids
    encode_s = time.perf_counter() - t
    # The same text as one batch of lines, which lets `tokenizers` use its threads.
    lines = text.splitlines(keepends=True)
    t = time.perf_counter()
    batch_tokens = sum(len(e.ids) for e in tok.encode_batch(lines))
    batch_s = time.perf_counter() - t
    rows.append({"tool": f"tokenizers {tokenizers.__version__}", "vocabulary_tokens": tok.get_vocab_size(),
                 "train_seconds": round(train_s, 2), "eval_tokens": len(ids), "encode_seconds": round(encode_s, 3),
                 "encode_seconds_as_lines": round(batch_s, 3), "eval_tokens_as_lines": batch_tokens,
                 "round_trip_exact": tok.decode(ids).encode("utf-8") == eval_bytes})
    print(rows[-1], file=sys.stderr)

    try:
        import tiktoken

        enc = tiktoken.get_encoding("cl100k_base")
        t = time.perf_counter()
        ids = enc.encode(text, disallowed_special=())
        rows.append({"tool": f"tiktoken {tiktoken.__version__} cl100k_base (pretrained, other data)", "vocabulary_tokens": enc.n_vocab,
                     "train_seconds": None, "eval_tokens": len(ids), "encode_seconds": round(time.perf_counter() - t, 3),
                     "round_trip_exact": enc.decode_bytes(ids) == eval_bytes})
        print(rows[-1], file=sys.stderr)
    except ImportError:
        pass

    report = {"train_bytes": args.train.stat().st_size, "eval_bytes": len(eval_bytes), "results": rows}
    out = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(out + "\n", encoding="utf-8")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
