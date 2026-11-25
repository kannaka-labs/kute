"""Layered vocabularies with local ids, against a flat vocabulary (D3).

    python benchmarks/layered_vocab.py KUTE BASE.kvf DOMAIN.kvf DOMAIN_TRAIN.txt DOMAIN_EVAL.txt WORK_DIR > report.json

An experiment, not a format. Nothing in the engine changes.

The question. Two peers already hold a general vocabulary (BASE). They
start exchanging text from a domain it was not trained on. They can:

  flat      train and distribute a new vocabulary for the domain (DOMAIN),
            and send plain token ids under it; or
  layered   keep BASE, and agree a small table of *local ids* on top of
            it: short ids for the tokens this domain (or this session)
            uses most, with a reversible fallback for the rest.

What a table is here. A list of BASE token ids in rank order. A token in
the table is sent as its rank; a token not in it is sent as the escape
rank (the table's length) followed by its BASE id. All numbers are
LEB128 varints, as in a KUTE container. Scopes measured:

  domain    one table, ranked by frequency in DOMAIN_TRAIN, sent once
  session   one table for each session, ranked by frequency in that
            session, sent with it and charged to it
  session over domain
            a session table of at most 127 entries that names tokens by
            their domain rank; everything else falls back to the domain
            table (and from there to the escape)

Every layered stream is turned back into BASE ids here and compared with
the original, so the fallback is shown to be reversible on this data and
not assumed. Sizes are token bytes only: no container header on either
side. A general compressor (zlib -9 on the token bytes of each session)
is applied to every row as well, because it may already do what local
ids do.

Fairness notes, so the rows are read rightly:
  * A flat vocabulary could also have its ids ranked by frequency. That
    row is here ("flat, ranked ids"): it is the same trick without
    layers, and what layering must be compared with.
  * The flat vocabulary was trained on DOMAIN_TRAIN. The layered side
    uses DOMAIN_TRAIN only to rank ids.
  * One domain, one base, one split into sessions by size.
"""
import base64
import collections
import hashlib
import json
import pathlib
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import zlib

SESSION_BYTES = (4096, 65536)
SESSION_TABLE = 127


def varint_len(n: int) -> int:
    size = 1
    while n >= 0x80:
        n >>= 7
        size += 1
    return size


def put(out: bytearray, n: int) -> None:
    while n >= 0x80:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    out.append(n)


def read_varints(data: bytes):
    ids, n, shift = [], 0, 0
    for byte in data:
        n |= (byte & 0x7F) << shift
        if byte & 0x80:
            shift += 7
        else:
            ids.append(n)
            n, shift = 0, 0
    assert shift == 0, "the token bytes end inside a number"
    return ids


def ids_of(container: bytes):
    """Token ids of a KUTE container: a JSON header, then varints (format
    1), or fixed-width little-endian ids inside one zlib stream (format 2,
    which is what the service answers with)."""
    assert container[:4] == b"KUTE" and container[4] in (1, 2), "not a KUTE container"
    start = container.index(b"{")
    header, length = json.JSONDecoder().raw_decode(container[start:].decode("latin-1"))
    body = container[start + length:]
    if container[4] == 1:
        ids = read_varints(body)
    else:
        raw = zlib.decompress(body)
        width = len(raw) // header["count"] if header["count"] else 1
        assert width * header["count"] == len(raw), "the ids are not of one width"
        ids = [int.from_bytes(raw[i:i + width], "little") for i in range(0, len(raw), width)]
    assert len(ids) == header["count"], (len(ids), header["count"])
    return ids


def fixed(ids) -> bytes:
    """Ids as `kute encode --compress` lays them out before zlib."""
    width = max(1, (max(ids, default=0).bit_length() + 7) // 8)
    return b"".join(i.to_bytes(width, "little") for i in ids)


def plain(ids) -> bytes:
    out = bytearray()
    for i in ids:
        put(out, i)
    return bytes(out)


def ranked(counter):
    """Ids by falling count, ties by id: the same order on both sides."""
    return [i for i, _ in sorted(counter.items(), key=lambda kv: (-kv[1], kv[0]))]


def table_bytes(table) -> int:
    return varint_len(len(table)) + sum(varint_len(i) for i in table)


def to_local(ids, table) -> bytes:
    rank = {token: r for r, token in enumerate(table)}
    escape = len(table)
    out = bytearray()
    for i in ids:
        r = rank.get(i)
        if r is None:
            put(out, escape)
            put(out, i)
        else:
            put(out, r)
    return bytes(out)


def from_local(data: bytes, table):
    numbers, out, escape, at = read_varints(data), [], len(table), 0
    while at < len(numbers):
        if numbers[at] == escape:
            out.append(numbers[at + 1])
            at += 2
        else:
            out.append(table[numbers[at]])
            at += 1
    return out


def to_two_level(ids, session_table, domain_table) -> bytes:
    """Session ranks first; then domain rank, moved up past the session
    table; then the escape and the base id."""
    session = {token: r for r, token in enumerate(session_table)}
    domain = {token: r for r, token in enumerate(domain_table)}
    offset = len(session_table)
    escape = offset + len(domain_table)
    out = bytearray()
    for i in ids:
        if i in session:
            put(out, session[i])
        elif i in domain:
            put(out, offset + domain[i])
        else:
            put(out, escape)
            put(out, i)
    return bytes(out)


def from_two_level(data: bytes, session_table, domain_table):
    numbers, out, at = read_varints(data), [], 0
    offset = len(session_table)
    escape = offset + len(domain_table)
    while at < len(numbers):
        n = numbers[at]
        if n == escape:
            out.append(numbers[at + 1])
            at += 2
        else:
            out.append(session_table[n] if n < offset else domain_table[n - offset])
            at += 1
    return out


def z(data: bytes) -> int:
    return len(zlib.compress(data, 9))


def sessions_of(text: bytes, size: int):
    """Pieces of at least `size` bytes, cut after a line end."""
    out, start = [], 0
    while start < len(text):
        end = text.find(b"\n", min(start + size, len(text)) - 1)
        end = len(text) if end < 0 else end + 1
        out.append(text[start:end])
        start = end
    return out


def main() -> int:
    if len(sys.argv) != 7:
        print(__doc__, file=sys.stderr)
        return 2
    kute, base_path, domain_path, train_path, eval_path, work = (pathlib.Path(a).resolve() for a in sys.argv[1:])
    work.mkdir(parents=True, exist_ok=True)
    # The service runs from a copy, so the build is not held open.
    binary = work / kute.name
    shutil.copyfile(kute, binary)

    def run(*args):
        done = subprocess.run([str(binary), *map(str, args)], capture_output=True, cwd=work)
        if done.returncode != 0:
            raise SystemExit(f"kute {' '.join(map(str, args))[:200]} failed: {done.stderr.decode(errors='replace')[:600]}")
        return done.stdout

    def encode_file(vocab, path, name):
        out = work / name
        out.unlink(missing_ok=True)
        run("encode", "--vocab", vocab, "--input", path, "--output", out, "--unknown", "escape")
        return ids_of(out.read_bytes())

    # Rank ids by how often the domain's training text uses them.
    started = time.perf_counter()
    base_train = collections.Counter(encode_file(base_path, train_path, "train-base.kute"))
    flat_train = collections.Counter(encode_file(domain_path, train_path, "train-flat.kute"))
    domain_table = ranked(base_train)
    flat_ranking = ranked(flat_train)

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    server = subprocess.Popen([str(binary), "serve", "--listen", f"127.0.0.1:{port}", "--vocab", str(base_path), "--vocab", str(domain_path), "--max-input-bytes", str(8 << 20), "--max-output-bytes", str(64 << 20)], cwd=work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        def call(path, body=None):
            request = urllib.request.Request(f"http://127.0.0.1:{port}/v1/{path}", data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=120) as answer:
                return json.loads(answer.read())

        for _ in range(200):
            try:
                capabilities = call("capabilities")
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise SystemExit("the service did not start")
        held = capabilities["vocabularies"]
        digests = {name: json.loads(run("inspect", path))["summary"]["digest"] for name, path in (("base", base_path), ("flat", domain_path))}
        names = {}
        for name, digest in digests.items():
            match = [v for v in held if v.get("digest") == digest]
            assert len(match) == 1, f"the service does not hold exactly one vocabulary with the digest of {name}"
            names[name] = match[0]["digest"]

        def encode(which, piece: bytes):
            answer = call("encode", {"vocabulary": names[which], "data_base64": base64.b64encode(piece).decode()})
            return ids_of(base64.b64decode(answer["container_base64"]))

        text = eval_path.read_bytes()
        report = {
            "question": "does a table of local ids over a held general vocabulary do as well as a flat vocabulary trained for the domain?",
            "kute_version": run("--version").decode().strip(),
            "base_vocabulary": {"file": base_path.name, "digest": digests["base"], "file_bytes": base_path.stat().st_size},
            "flat_vocabulary": {"file": domain_path.name, "digest": digests["flat"], "file_bytes": domain_path.stat().st_size},
            "domain_train": {"file": train_path.name, "bytes": train_path.stat().st_size, "sha256": hashlib.sha256(train_path.read_bytes()).hexdigest()},
            "domain_eval": {"file": eval_path.name, "bytes": len(text), "sha256": hashlib.sha256(text).hexdigest()},
            "sent_once": {
                "flat_vocabulary_file_bytes": domain_path.stat().st_size,
                "flat_vocabulary_file_zlib_bytes": z(domain_path.read_bytes()),
                "flat_ranking_table_bytes": table_bytes(flat_ranking),
                "domain_table_bytes": table_bytes(domain_table),
                "domain_table_entries": len(domain_table),
            },
            "by_session_size": [],
        }
        for size in SESSION_BYTES:
            pieces = sessions_of(text, size)
            assert b"".join(pieces) == text
            rows = collections.OrderedDict((key, {"token_bytes": 0, "zlib_token_bytes": 0}) for key in ("flat, plain ids", "flat, ranked ids", "base, plain ids", "layered: domain table", "layered: session table", "layered: session over domain"))
            tokens = {"flat": 0, "base": 0}
            escapes = {"layered: domain table": 0}
            session_table_bytes = {"layered: session table": 0, "layered: session over domain": 0}
            domain_rank = {token: r for r, token in enumerate(domain_table)}
            for piece in pieces:
                flat_ids, base_ids = encode("flat", piece), encode("base", piece)
                tokens["flat"] += len(flat_ids)
                tokens["base"] += len(base_ids)
                streams = {
                    "flat, plain ids": plain(flat_ids),
                    "flat, ranked ids": to_local(flat_ids, flat_ranking),
                    "base, plain ids": plain(base_ids),
                    "layered: domain table": to_local(base_ids, domain_table),
                }
                assert from_local(streams["flat, ranked ids"], flat_ranking) == flat_ids
                assert from_local(streams["layered: domain table"], domain_table) == base_ids
                escapes["layered: domain table"] += sum(1 for i in base_ids if i not in domain_rank)
                # A table of the session's own, in full, sent with it.
                own = ranked(collections.Counter(base_ids))
                stream = to_local(base_ids, own)
                assert from_local(stream, own) == base_ids
                streams["layered: session table"] = stream
                session_table_bytes["layered: session table"] += table_bytes(own)
                # A short session table over the domain table. It names
                # tokens by domain rank where they have one.
                top = own[:SESSION_TABLE]
                stream = to_two_level(base_ids, top, domain_table)
                assert from_two_level(stream, top, domain_table) == base_ids
                streams["layered: session over domain"] = stream
                session_table_bytes["layered: session over domain"] += varint_len(len(top)) + sum(varint_len(domain_rank[t]) if t in domain_rank else varint_len(len(domain_table)) + varint_len(t) for t in top)
                for key, stream in streams.items():
                    rows[key]["token_bytes"] += len(stream)
                    rows[key]["zlib_token_bytes"] += z(stream)
                # What `kute encode --compress` stores for the tokens.
                for key, ids in (("flat, plain ids", flat_ids), ("base, plain ids", base_ids)):
                    rows[key]["zlib_fixed_width_bytes"] = rows[key].get("zlib_fixed_width_bytes", 0) + z(fixed(ids))
            for key, extra in session_table_bytes.items():
                rows[key]["session_table_bytes"] = extra
                rows[key]["token_bytes_with_tables"] = rows[key]["token_bytes"] + extra
                rows[key]["zlib_token_bytes_with_tables"] = rows[key]["zlib_token_bytes"] + extra
            report["by_session_size"].append({
                "session_bytes_at_least": size,
                "sessions": len(pieces),
                "text_bytes": len(text),
                "zlib_9_of_the_text_each_session": sum(z(p) for p in pieces),
                "tokens": tokens,
                "base_tokens_not_in_domain_table": escapes["layered: domain table"],
                "every_layered_stream_decoded_to_the_same_ids": True,
                "rows": rows,
            })
    finally:
        server.kill()
        server.wait()
    report["seconds"] = round(time.perf_counter() - started, 1)
    print(json.dumps(report, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
