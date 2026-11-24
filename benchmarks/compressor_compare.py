"""Sizes from general-purpose compressors on one evaluation file, for
comparison with a KUTE container of the same data.

    python benchmarks/compressor_compare.py EVAL_FILE [--train TRAIN_FILE]
        [--kute-container BYTES] [--kute-vocabulary BYTES] [--output report.json]
        [--event-bytes] [--record-formats]

Every compressor sees the evaluation file whole. With --train, Zstandard is
also run with a dictionary trained on the training file: that is the closest
rival to a shared vocabulary, since both sides hold something learned from
other data in advance. The dictionary's size is reported and counted the same
way the vocabulary's is.

With --event-bytes the evaluation file is taken as event traces, one record
per line with event names separated by spaces, and the compressors are also
run on a form that costs them nothing to parse: one byte per event and a zero
byte at the end of each record. Ratios for those rows are still against the
text file. It needs at most 255 distinct event names.

With --record-formats the same traces are also written in binary record
formats, one record per line of the input, and each is reported raw and
under zstd -19 and xz -9: CBOR and MessagePack (an array of records, each an
array of event names, and again with each name replaced by a small integer),
and the Protocol Buffers wire format for `message Trace { repeated string
event = 1; }` and for `message Trace { repeated uint32 event = 1; }` (packed),
each record length-prefixed. The Protocol Buffers bytes are written by this
script, not by protoc. The integer forms need a name table that is not
counted; it is a few hundred bytes. Needs `cbor2` and `msgpack`.

Needs the `zstandard` and `lz4` packages. Sizes only; nothing is timed.
"""
import argparse
import gzip
import hashlib
import json
import lzma
import pathlib
import sys

import lz4.frame
import zstandard

DICT_SIZES = [112_640, 1_048_576]
SAMPLE_BYTES = 64 * 1024
MAX_TRAIN_BYTES = 256 * 1024 * 1024


def samples_of(path: pathlib.Path) -> list[bytes]:
    """The training file cut into 64 KiB pieces at line ends."""
    out, total = [], 0
    with open(path, "rb") as f:
        while total < MAX_TRAIN_BYTES:
            chunk = f.read(SAMPLE_BYTES)
            if not chunk:
                break
            chunk += f.readline()
            out.append(chunk)
            total += len(chunk)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("eval_file", type=pathlib.Path)
    ap.add_argument("--train", type=pathlib.Path)
    ap.add_argument("--kute-container", type=int)
    ap.add_argument("--kute-vocabulary", type=int)
    ap.add_argument("--output", type=pathlib.Path)
    ap.add_argument("--event-bytes", action="store_true")
    ap.add_argument("--record-formats", action="store_true")
    args = ap.parse_args()

    data = args.eval_file.read_bytes()
    raw = len(data)
    rows = []

    def row(name, size, shared=0, note=None):
        rows.append({
            "method": name,
            "bytes": size,
            "ratio": round(raw / size, 3),
            "shared_bytes": shared,
            "ratio_with_shared": round(raw / (size + shared), 3),
            **({"note": note} if note else {}),
        })

    row("lz4 (default)", len(lz4.frame.compress(data)))
    row("lz4 (level 12)", len(lz4.frame.compress(data, compression_level=12)))
    row("gzip -9", len(gzip.compress(data, compresslevel=9)))
    row("zstd -3", len(zstandard.ZstdCompressor(level=3).compress(data)))
    row("zstd -19", len(zstandard.ZstdCompressor(level=19).compress(data)))
    params = zstandard.ZstdCompressionParameters.from_level(22, enable_ldm=True, window_log=27)
    row("zstd -22 --long=27", len(zstandard.ZstdCompressor(compression_params=params).compress(data)))
    row("xz -9", len(lzma.compress(data, preset=9)))
    packed_info = None
    if args.event_bytes:
        names, packed = {}, bytearray()
        for line in data.splitlines():
            for name in line.split():
                packed.append(names.setdefault(name, len(names) + 1))
            packed.append(0)
        packed = bytes(packed)
        packed_info = {"bytes": len(packed), "event_names": len(names)}
        note = "one byte per event, zero byte per record end; ratio is against the text file"
        row("gzip -9 on event bytes", len(gzip.compress(packed, compresslevel=9)), note=note)
        row("zstd -19 on event bytes", len(zstandard.ZstdCompressor(level=19).compress(packed)), note=note)
        row("zstd -22 --long=27 on event bytes", len(zstandard.ZstdCompressor(compression_params=params).compress(packed)), note=note)
        row("xz -9 on event bytes", len(lzma.compress(packed, preset=9)), note=note)
    if args.record_formats:
        import cbor2
        import msgpack

        names = {}
        records = [line.split() for line in data.splitlines()]
        numbered = [[names.setdefault(n, len(names) + 1) for n in r] for r in records]
        text_records = [[n.decode("utf-8") for n in r] for r in records]

        def varint(n):
            out = bytearray()
            while n >= 0x80:
                out.append(n & 0x7F | 0x80)
                n >>= 7
            out.append(n)
            return bytes(out)

        TAG = bytes([0x0A])  # field 1, length-delimited

        def proto_strings(r):
            body = b"".join(TAG + varint(len(n)) + n for n in r)
            return varint(len(body)) + body

        def proto_packed(r):
            packed = b"".join(varint(n) for n in r)
            body = TAG + varint(len(packed)) + packed if r else b""
            return varint(len(body)) + body

        forms = [
            ("CBOR, names", cbor2.dumps(text_records)),
            ("CBOR, integers", cbor2.dumps(numbered)),
            ("MessagePack, names", msgpack.packb(text_records)),
            ("MessagePack, integers", msgpack.packb(numbered)),
            ("Protocol Buffers, repeated string", b"".join(map(proto_strings, records))),
            ("Protocol Buffers, packed uint32", b"".join(map(proto_packed, numbered))),
        ]
        note = "binary record form of the same traces; ratio is against the text file"
        for name, blob in forms:
            row(name, len(blob), note=note)
            row(f"{name} + zstd -19", len(zstandard.ZstdCompressor(level=19).compress(blob)), note=note)
            row(f"{name} + xz -9", len(lzma.compress(blob, preset=9)), note=note)
    train_info = None
    if args.train:
        samples = samples_of(args.train)
        train_info = {"path": str(args.train), "bytes_used": sum(map(len, samples)), "samples": len(samples)}
        for size in DICT_SIZES:
            try:
                dictionary = zstandard.train_dictionary(size, samples, level=19)
            except zstandard.ZstdError as e:
                rows.append({"method": f"zstd -19 with a {size}-byte trained dictionary", "error": str(e)})
                continue
            used = len(dictionary.as_bytes())
            compressed = len(zstandard.ZstdCompressor(level=19, dict_data=dictionary).compress(data))
            row(f"zstd -19 with a trained dictionary ({used:,} bytes)", compressed, shared=used)
    if args.kute_container:
        row("KUTE compressed container", args.kute_container, shared=args.kute_vocabulary or 0,
            note="sizes supplied on the command line from a kute run on the same file")

    report = {
        "eval_file": {"path": str(args.eval_file), "bytes": raw, "sha256": hashlib.sha256(data).hexdigest()},
        "train_file": train_info,
        **({"event_bytes": packed_info} if packed_info else {}),
        "versions": {"python": sys.version.split()[0], "zstandard": zstandard.__version__,
                     "libzstd": ".".join(map(str, zstandard.ZSTD_VERSION)), "lz4": lz4.__version__},
        "what": "bytes: compressed size of the evaluation file. shared_bytes: what both sides must already hold "
                "(a dictionary or a vocabulary). ratio = raw / bytes; ratio_with_shared = raw / (bytes + shared_bytes).",
        "results": sorted(rows, key=lambda r: r.get("bytes", 1 << 62)),
    }
    text = json.dumps(report, indent=2)
    if args.output:
        args.output.write_text(text + "\n", encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
