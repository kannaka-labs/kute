"""Independent implementation of the KVF 0.1 content digest.

Written from the specification (specifications/kvf/KVF-0.1.md, section 4),
with the Python standard library only and no KUTE code. It exists to show
that a vocabulary's identity can be computed and checked by a second
implementation, and it is run in CI against the vectors in
specifications/kvf/vectors and against freshly trained vocabularies.

    python conformance/kvf_digest.py FILE.kvf [FILE.kvf ...]

Prints one line per file. Exit status 0 only if every file carries a digest
and it equals the one computed here.
"""
import hashlib
import json
import sys

MANIFEST = [
    ("kvf_version", True), ("id", True), ("version", True), ("algorithm", True),
    ("modality", True), ("payload_kind", True), ("lossless", True),
    ("description", False), ("created", False), ("generator", True),
]


class Unsupported(Exception):
    pass


def text(value):
    """A JSON string as the canonical form writes it."""
    if not isinstance(value, str):
        raise Unsupported(f"expected a string, got {type(value).__name__}")
    # ensure_ascii=False: characters above U+007F are written as UTF-8.
    # Python escapes ", \\ and U+0000..U+001F exactly as the canonical form
    # requires (\\b \\f \\n \\r \\t, otherwise \\u00xx in lower case).
    return json.dumps(value, ensure_ascii=False)


def integer(value):
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise Unsupported(f"expected a non-negative integer, got {value!r}")
    return str(value)


def boolean(value):
    if not isinstance(value, bool):
        raise Unsupported(f"expected a boolean, got {value!r}")
    return "true" if value else "false"


def free(value):
    """A free-form JSON value: object keys sorted by Unicode code point."""
    if value is None:
        return "null"
    if isinstance(value, bool):
        return boolean(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        raise Unsupported("non-integer numbers have no canonical form in KVF 0.1")
    if isinstance(value, str):
        return text(value)
    if isinstance(value, list):
        return "[" + ",".join(free(v) for v in value) + "]"
    if isinstance(value, dict):
        # Rust orders keys by UTF-8 bytes, which is code point order.
        keys = sorted(value, key=lambda k: k.encode("utf-8"))
        return "{" + ",".join(text(k) + ":" + free(value[k]) for k in keys) + "}"
    raise Unsupported(f"cannot serialize {type(value).__name__}")


def obj(pairs):
    return "{" + ",".join(text(k) + ":" + v for k, v in pairs) + "}"


def present(d, key):
    return key in d and d[key] is not None


def payload(d):
    """The one payload key of a token or test vector."""
    if "bytes" in d:
        return ("bytes", text(d["bytes"]))
    if "symbols" in d:
        return ("symbols", "[" + ",".join(text(s) for s in d["symbols"]) + "]")
    raise Unsupported("neither bytes nor symbols")


def canonical(doc):
    out = []
    m = doc["manifest"]
    out.append(("manifest", obj([(k, boolean(m[k]) if k == "lossless" else text(m[k])) for k, required in MANIFEST if required or present(m, k)])))
    if present(doc, "pretokenizer"):
        out.append(("pretokenizer", text(doc["pretokenizer"])))
    if present(doc, "adapter"):
        a = doc["adapter"]
        pairs = [("name", text(a["name"]))]
        options = a.get("options") or {}
        if options:
            pairs.append(("options", obj([(k, text(options[k])) for k in sorted(options, key=lambda k: k.encode("utf-8"))])))
        pairs.append(("lossless", boolean(a["lossless"])))
        out.append(("adapter", obj(pairs)))
    tokens = []
    for t in doc["tokens"]:
        pairs = [("id", integer(t["id"])), ("kind", text(t["kind"])), payload(t)]
        if present(t, "frequency"):
            pairs.append(("frequency", integer(t["frequency"])))
        tokens.append(obj(pairs))
    out.append(("tokens", "[" + ",".join(tokens) + "]"))
    merges = [obj([("left", integer(r["left"])), ("right", integer(r["right"])), ("id", integer(r["id"]))]) for r in doc.get("merges", [])]
    out.append(("merges", "[" + ",".join(merges) + "]"))
    statistics = doc.get("statistics", {})
    out.append(("statistics", obj([(k, integer(statistics[k])) for k in sorted(statistics, key=lambda k: k.encode("utf-8"))])))
    p = doc.get("provenance", {})
    pairs = [("corpus_fingerprints", "[" + ",".join(text(f) for f in p.get("corpus_fingerprints", [])) + "]")]
    if present(p, "license"):
        pairs.append(("license", text(p["license"])))
    if present(p, "training"):
        pairs.append(("training", free(p["training"])))
    out.append(("provenance", obj(pairs)))
    vectors = []
    for v in doc.get("test_vectors", []):
        vectors.append(obj([payload(v), ("ids", "[" + ",".join(integer(i) for i in v["ids"]) + "]")]))
    out.append(("test_vectors", "[" + ",".join(vectors) + "]"))
    representations = doc.get("representations") or []
    if representations:
        items = []
        for r in representations:
            pairs = [("mapping_id", text(r["mapping_id"])), ("mapping_version", text(r["mapping_version"])), ("kind", text(r["kind"]))]
            if present(r, "uri"):
                pairs.append(("uri", text(r["uri"])))
            items.append(obj(pairs))
        out.append(("representations", "[" + ",".join(items) + "]"))
    return obj(out).encode("utf-8")


def digest(doc):
    return "sha256:" + hashlib.sha256(canonical(doc)).hexdigest()


def main(paths):
    if not paths:
        print(__doc__)
        return 2
    failed = 0
    for path in paths:
        with open(path, "rb") as f:
            doc = json.loads(f.read().decode("utf-8"))
        recorded = (doc.get("integrity") or {}).get("digest")
        try:
            computed = digest(doc)
        except (Unsupported, KeyError) as e:
            print(f"FAIL {path}: {e}")
            failed += 1
            continue
        if recorded == computed:
            print(f"ok   {path} {computed}")
        else:
            print(f"FAIL {path}: recorded {recorded}, computed {computed}")
            failed += 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
