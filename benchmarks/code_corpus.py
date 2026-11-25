"""Build a source-code corpus from an unpacked CPython source tree.

    curl -LO https://www.python.org/ftp/python/3.12.7/Python-3.12.7.tgz
    tar -xzf Python-3.12.7.tgz --wildcards 'Python-3.12.7/Lib/*.py'
    python benchmarks/code_corpus.py Python-3.12.7/Lib OUT_DIR

Every `.py` file under the directory is taken in sorted path order. Files
that are not valid UTF-8 are left out (the standard library's tests hold a
few on purpose) and counted. Every tenth file goes to `eval.txt`, the rest
to `train.txt`, whole files, joined as they are: no file is in both, so the
evaluation text was not seen in training, though code in one file can of
course resemble code in another. `manifest.json` records the counts and
the SHA-256 of both outputs; the same tree gives the same bytes.
"""
import hashlib
import json
import pathlib
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    root, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    files = sorted(root.rglob("*.py"), key=lambda p: p.relative_to(root).as_posix())
    parts = {"train": [], "eval": []}
    counts = {"train": 0, "eval": 0, "skipped_not_utf8": 0}
    kept = 0
    for path in files:
        data = path.read_bytes()
        try:
            data.decode("utf-8")
        except UnicodeDecodeError:
            counts["skipped_not_utf8"] += 1
            continue
        side = "eval" if kept % 10 == 9 else "train"
        kept += 1
        parts[side].append(data)
        counts[side] += 1
    manifest = {"source": root.as_posix(), "files": counts}
    for side, chunks in parts.items():
        data = b"".join(chunks)
        (out / f"{side}.txt").write_bytes(data)
        manifest[side] = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
