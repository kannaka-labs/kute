"""Encode one text file with KUTE and with tiktoken, on the same vocabulary.

Both are called from Python, so both pay for building a Python list of ids.
The KUTE vocabulary is exported to tiktoken's format and loaded by tiktoken
itself; the script fails if the two produce different ids.

    python benchmarks/tiktoken_compare.py vocabulary.kvf held-out.txt

Set RAYON_NUM_THREADS=1 to hold KUTE to one thread. tiktoken encodes a single
document on one thread.
"""
import os
import statistics
import sys
import tempfile
import time

import kute
import tiktoken
from tiktoken.load import load_tiktoken_bpe


def timed(fn):
    start = time.perf_counter()
    result = fn()
    return time.perf_counter() - start, result


def main():
    vocab_path, text_path = sys.argv[1], sys.argv[2]
    with open(text_path, "rb") as f:
        raw = f.read()
    text = raw.decode("utf-8")
    megabytes = len(raw) / 1e6

    engine = kute.Engine()
    vocabulary = kute.Vocabulary.load(vocab_path)
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "v.tiktoken")
        kwargs = vocabulary.export_tiktoken(path)
        reference = tiktoken.Encoding(mergeable_ranks=load_tiktoken_bpe(path), **kwargs)

    # Alternate the two so that drift in machine load hits both alike.
    kute_times, tik_times = [], []
    for _ in range(7):
        seconds, kute_ids = timed(lambda: engine.encode(text, vocabulary))
        kute_times.append(seconds)
        seconds, tik_ids = timed(lambda: reference.encode_ordinary(text))
        tik_times.append(seconds)
    kute_s, tik_s = statistics.median(kute_times), statistics.median(tik_times)
    if kute_ids != tik_ids:
        sys.exit("FAIL: KUTE and tiktoken produced different ids")

    print(f"kute {kute.__version__}, tiktoken {tiktoken.__version__}, {megabytes:.1f} MB, {len(kute_ids)} tokens, ids identical")
    print(f"kute threads: {os.environ.get('RAYON_NUM_THREADS', 'all')}")
    print(f"kute:     {megabytes / kute_s:6.2f} MB/s (median of 7, alternating)")
    print(f"tiktoken: {megabytes / tik_s:6.2f} MB/s (median of 7, one thread)")


if __name__ == "__main__":
    main()
