"""QueenSync: two agents, ordinary structured messages, one learned vocabulary.

    python examples/queensync/queensync.py KUTE_BINARY WORK_DIR > report.json

Runs the whole loop of the v0.7 specification's E2 with the released
commands and no change to the engine:

  1. a queen and a worker exchange JSON messages to complete tasks;
  2. a vocabulary is trained on the first traces (`kute train`);
  3. the work changes: new procedures appear. The traces are observed
     (`kute discover`) and the recurring runs proposed as tokens
     (`kute evolve`);
  4. the candidate is judged on held-out tasks (`kute shadow`);
  5. if approved it is proposed to the other agent over the connection
     (`kute peer send --propose`), and the remaining tasks are exchanged
     under it; if rejected they are exchanged under the old one;
  6. tasks with message kinds nobody has seen are carried by escape.

Both agents are scripted and the workload is SYNTHETIC: seeded templates
with random values. No language model is called anywhere. Every byte
between the two processes passes through a relay in this script that
counts it. The connection is plain loopback so that the count is the
protocol's own bytes; authenticated TLS between peers is exercised by
the engine's tests, not here.

Each message is carried as its canonical JSON cut into symbols: one for
each brace, comma and key (`{`, `,`, `"op":`), and one for each character
of a value. A decoded task is the same bytes that were sent.
"""
import json
import lzma
import pathlib
import random
import socket
import subprocess
import sys
import threading
import time
import zlib

SEED = 20261125
TRAIN, OBSERVE, SHADOW, LIVE = 400, 300, 300, 1000
BATCH = 100
VOCAB_SIZE = 512
TOP_PATTERNS = 32
NOVEL_SHARE = 0.02


# ---------------------------------------------------------------- workload

WORDS = "alpha bravo delta echo gamma kilo lima metro nova omega pixel quartz radar sigma tango umbra vector whisky xenon zulu".split()
TOOLS_OLD = {"lookup": ["search"], "ingest": ["fetch", "parse"], "summarize": ["fetch", "condense"]}
TOOLS_NEW = {"audit": ["lock", "verify", "sign", "unlock"], "reconcile": ["fetch", "diff", "patch", "verify"]}


def task(rng, number, kinds, novel=False):
    """One task: the messages the queen and the worker exchange for it."""
    kind = rng.choice(sorted(kinds))
    tid = f"t{number:06d}"
    subject = f"{rng.choice(WORDS)} {rng.choice(WORDS)}"
    messages = [{"from": "queen", "to": "worker", "op": "assign", "task": tid, "kind": kind, "subject": subject, "priority": rng.choice(["low", "normal", "high"])}]
    for step, tool in enumerate(kinds[kind]):
        messages.append({"from": "worker", "to": "queen", "op": "tool.call", "task": tid, "step": step, "tool": tool, "args": {"target": f"store://{rng.choice(WORDS)}/{rng.randrange(40)}", "limit": rng.choice([10, 50, 100])}})
        messages.append({"from": "queen", "to": "worker", "op": "tool.result", "task": tid, "step": step, "tool": tool, "ok": True, "items": rng.randrange(500), "ms": rng.randrange(5, 2000)})
    if novel:
        # A message kind no vocabulary has seen, with free text.
        messages.append({"from": "worker", "to": "queen", "op": "escalate", "task": tid, "reason": f"{rng.choice(WORDS)} {rng.choice(WORDS)} {rng.choice(WORDS)} needs a decision", "severity": rng.randrange(1, 6)})
        messages.append({"from": "queen", "to": "worker", "op": "ruling", "task": tid, "proceed": rng.choice([True, False])})
    messages.append({"from": "worker", "to": "queen", "op": "report", "task": tid, "status": "done", "steps": len(kinds[kind]), "digest": f"{rng.getrandbits(64):016x}"})
    return messages


def canonical(messages):
    return "".join(json.dumps(m, sort_keys=True, separators=(",", ":")) + "\n" for m in messages).encode()


def lexemes(value):
    """The canonical JSON of `value` as lexemes whose concatenation is it."""
    if isinstance(value, dict):
        out = ["{"]
        for i, key in enumerate(sorted(value)):
            if i:
                out.append(",")
            out.append(json.dumps(key) + ":")
            out.extend(lexemes(value[key]))
        return out + ["}"]
    if isinstance(value, list):
        out = ["["]
        for i, item in enumerate(value):
            if i:
                out.append(",")
            out.extend(lexemes(item))
        return out + ["]"]
    # A value is spelled out, a character a symbol: values are open-ended
    # (ids, numbers, free text), and a symbol for each would be an
    # alphabet without end. Common values become tokens by training.
    return list(json.dumps(value))


def as_events(messages):
    """A task as the JSON Lines of events `kute` reads: one lexeme a line.
    Spaces are written %20 so that a run of symbols can be named on a
    command line; NL ends a message."""
    lines = []
    for message in messages:
        for lexeme in lexemes(message) + ["NL"]:
            lines.append(json.dumps({"event": lexeme.replace("%", "%25").replace(" ", "%20")}, separators=(",", ":")))
    return ("\n".join(lines) + "\n").encode()


def from_events(data):
    out = []
    for line in data.decode().splitlines():
        symbol = json.loads(line)["event"]
        out.append("\n" if symbol == "NL" else symbol.replace("%20", " ").replace("%25", "%"))
    return "".join(out).encode()


# ---------------------------------------------------------------- transport

def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Relay(threading.Thread):
    """Passes one connection through and counts its bytes each way.
    With `cut_after`, drops the connection once that many have passed."""

    def __init__(self, upstream_port, cut_after=None):
        super().__init__(daemon=True)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.port = self.listener.getsockname()[1]
        self.upstream_port = upstream_port
        self.cut_after = cut_after
        self.to_server = 0
        self.to_client = 0
        self.lock = threading.Lock()

    def run(self):
        client, _ = self.listener.accept()
        self.listener.close()
        server = None
        for _ in range(200):
            try:
                server = socket.create_connection(("127.0.0.1", self.upstream_port), timeout=1)
                break
            except OSError:
                time.sleep(0.05)
        if server is None:
            client.close()
            return
        server.settimeout(None)

        def pump(source, sink, name):
            try:
                while True:
                    data = source.recv(65536)
                    if not data:
                        break
                    with self.lock:
                        setattr(self, name, getattr(self, name) + len(data))
                        over = self.cut_after is not None and self.to_server + self.to_client >= self.cut_after
                    if over:
                        break
                    sink.sendall(data)
            except OSError:
                pass
            for s in (source, sink):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

        back = threading.Thread(target=pump, args=(server, client, "to_client"), daemon=True)
        back.start()
        pump(client, server, "to_server")
        back.join(timeout=10)
        client.close()
        server.close()


class Bench:
    def __init__(self, kute, work):
        self.kute = str(pathlib.Path(kute).resolve())
        self.work = pathlib.Path(work).resolve()
        self.work.mkdir(parents=True, exist_ok=True)

    def run(self, *args, check=True):
        done = subprocess.run([self.kute, *map(str, args)], cwd=self.work, capture_output=True)
        if check and done.returncode != 0:
            raise SystemExit(f"kute {' '.join(map(str, args))[:300]} failed: {done.stderr.decode(errors='replace')[:800]}")
        return done

    def exchange(self, name, vocab, held, tasks, propose=None, cut_after=None):
        """One connection: the worker's side serves, the queen's side sends
        every task in `tasks` (name, messages). Returns what was measured."""
        out_dir = self.work / f"recv-{name}"
        port = free_port()
        serve = [self.kute, "peer", "serve", "--listen", f"127.0.0.1:{port}", "--insecure-plain", "--accept-vocabularies", "--accept-proposals", "--output-dir", str(out_dir), "--timeout", "20"]
        for path in held:
            serve += ["--vocab", str(path)]
        server = subprocess.Popen(serve, cwd=self.work, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        relay = Relay(port, cut_after)
        relay.start()
        send = ["peer", "send", "--connect", f"127.0.0.1:{relay.port}", "--insecure-plain", "--vocab", vocab, "--unknown", "escape"]
        if propose:
            for run in propose["additions"]:
                send += ["--propose", " ".join(run)]
            send += ["--new-version", propose["version"]]
        if tasks:
            send += ["--input", *[f"tasks/{task_name}.jsonl" for task_name, _ in tasks]]
        started = time.perf_counter()
        sent = self.run(*send, check=False)
        seconds = time.perf_counter() - started
        try:
            server_out, _ = server.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()
            server_out, _ = server.communicate()
        relay.join(timeout=15)
        result = {"socket_bytes_to_worker": relay.to_server, "socket_bytes_to_queen": relay.to_client, "seconds": round(seconds, 3), "sender_ok": sent.returncode == 0, "completed": 0, "escaped_symbols": 0, "received_vocabularies": []}
        if server.returncode == 0 and server_out.strip():
            report = json.loads(server_out)
            result["received_vocabularies"] = sorted(str(p) for p in out_dir.glob("*.kvf")) if out_dir.exists() else []
            for (task_name, messages), got in zip(tasks, report["received"]):
                decoded = (out_dir / f"message-{got['id']}.bin").read_bytes()
                result["escaped_symbols"] += got["escaped_symbols"]
                # A task is complete when what the worker decoded is, byte
                # for byte, the messages the queen sent.
                if from_events(decoded) == canonical(messages):
                    result["completed"] += 1
        return result

    def payload(self, vocab, tasks):
        """Container bytes for each task, by `kute encode`: with and
        without `--compress`, and how the plain container divides between
        its header (which names the vocabulary) and its tokens."""
        sizes = {"compressed": 0, "plain": 0, "plain_header": 0, "plain_tokens": 0}
        tmp = self.work / "tmp.kute"
        for task_name, _ in tasks:
            tmp.unlink(missing_ok=True)
            self.run("encode", "--vocab", vocab, "--input", f"tasks/{task_name}.jsonl", "--output", "tmp.kute", "--compress", "--unknown", "escape")
            sizes["compressed"] += tmp.stat().st_size
            tmp.unlink()
            self.run("encode", "--vocab", vocab, "--input", f"tasks/{task_name}.jsonl", "--output", "tmp.kute", "--unknown", "escape")
            plain = tmp.read_bytes()
            # The header is one JSON object after the magic; the tokens follow it.
            start = plain.index(b"{")
            _, length = json.JSONDecoder().raw_decode(plain[start:].decode("latin-1"))
            sizes["plain"] += len(plain)
            sizes["plain_header"] += start + length
            sizes["plain_tokens"] += len(plain) - start - length
        return sizes


def chunks(items, size):
    return [items[i:i + size] for i in range(0, len(items), size)]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    bench = Bench(sys.argv[1], sys.argv[2])
    work = bench.work
    (work / "tasks").mkdir(exist_ok=True)
    rng = random.Random(SEED)
    everything = {**TOOLS_OLD, **TOOLS_NEW}
    sets, number = {}, 0
    for name, count, kinds in (("train", TRAIN, TOOLS_OLD), ("observe", OBSERVE, everything), ("shadow", SHADOW, everything), ("live", LIVE, everything)):
        sets[name] = []
        for _ in range(count):
            novel = name == "live" and rng.random() < NOVEL_SHARE
            messages = task(rng, number, kinds, novel)
            task_name = f"{name[0]}{number:05d}"
            (work / "tasks" / f"{task_name}.jsonl").write_bytes(as_events(messages))
            sets[name].append((task_name, messages))
            number += 1
    assert all(from_events(as_events(m)) == canonical(m) for s in sets.values() for _, m in s)

    def joined(name, parts):
        path = work / f"{name}.jsonl"
        path.write_bytes(b"".join(as_events(m) for part in parts for _, m in sets[part]))
        return path

    # 2. The first vocabulary, from the first traces.
    bench.run("train", "--input", joined("train", ["train"]).name, "--id", "example:queensync", "--algorithm", "sequence-bpe", "--adapter", "jsonl-events", "--vocab-size", VOCAB_SIZE, "--output", "base.kvf", "--force")

    # 3. The work changes. Exchange it under the vocabulary in use, then
    # look at what was exchanged.
    observed = [bench.exchange(f"observe-{i}", "base.kvf", [], batch) for i, batch in enumerate(chunks(sets["observe"], BATCH))]
    bench.run("discover", "--input", joined("observe", ["observe"]).name, "--top", TOP_PATTERNS, "--output", "discover.json")
    found = json.loads((work / "discover.json").read_text(encoding="utf-8"))["discovery"]["patterns"]
    additions = [p["symbols"] for p in found if p["kind"] == "contiguous"]
    flags = [x for run in additions for x in ("--add", " ".join(run))]
    bench.run("evolve", "--vocab", "base.kvf", *flags, "--new-version", "0.2.0", "--output", "candidate.kvf", "--force")

    # 4. Judge the candidate on tasks neither vocabulary was made from.
    horizon = LIVE / SHADOW
    shadow = json.loads(bench.run("shadow", "--parent", "base.kvf", "--candidate", "candidate.kvf", "--horizon", f"{horizon:.4f}", "--input", *[f"tasks/{n}.jsonl" for n, _ in sets["shadow"]]).stdout)
    approved = shadow["decision"] == "approve"

    # 5. Negotiate, on a connection that carries nothing else. A real
    # deployment would do this only if the candidate was approved. It is
    # done here whatever the decision, and labelled, so that the path is
    # exercised on every run and not only when the data happens to approve.
    negotiation = None
    if True:
        negotiation = bench.exchange("negotiate", "base.kvf", ["base.kvf"], [], propose={"additions": additions, "version": "0.2.0"})
        received = negotiation["received_vocabularies"]
        assert received, "the worker did not keep the proposed vocabulary"
        # The worker's copy and the queen's are the same vocabulary.
        ours = json.loads(bench.run("inspect", "candidate.kvf").stdout)
        theirs = json.loads(bench.run("inspect", received[0]).stdout)
        negotiation["same_digest_on_both_sides"] = ours["summary"]["digest"] == theirs["summary"]["digest"]
        negotiation["digest"] = ours["summary"]["digest"]
        negotiation["ran_because"] = "the candidate was approved" if approved else "forced, to exercise the path: the decision was reject and a deployment would not have negotiated"
    # The first connection of all carries the base vocabulary to the worker.
    first_contact = bench.exchange("first", "base.kvf", [], [])

    # 6. The live tasks, under both vocabularies: the one the decision
    # chose is what would run; the other shows whether it chose well.
    # Batch by batch, and which goes first alternates, so that neither
    # is always the one that pays for a cold start.
    sides = {"base": ("base.kvf", ["base.kvf"]), "candidate": ("candidate.kvf", ["base.kvf", "candidate.kvf"])}
    ran = {"base": [], "candidate": []}
    for i, batch in enumerate(chunks(sets["live"], BATCH)):
        for label in (("base", "candidate") if i % 2 == 0 else ("candidate", "base")):
            ran[label].append(bench.exchange(f"live-{label}-{i}", sides[label][0], sides[label][1], batch))

    def carry(label):
        batches, vocab = ran[label], sides[label][0]
        completed = sum(b["completed"] for b in batches)
        socket_bytes = sum(b["socket_bytes_to_worker"] + b["socket_bytes_to_queen"] for b in batches)
        return {
            "tasks": LIVE,
            "completed": completed,
            "completion_rate": completed / LIVE,
            "socket_bytes": socket_bytes,
            "socket_bytes_per_completed_task": round(socket_bytes / max(completed, 1), 2),
            "escaped_symbols": sum(b["escaped_symbols"] for b in batches),
            "seconds_per_batch_of_100": sorted(b["seconds"] for b in batches)[len(batches) // 2],
            "container_bytes_one_task_each": bench.payload(vocab, sets["live"]),
        }

    live = {"base": carry("base"), "candidate": carry("candidate")}

    # Recovery: the connection is dropped part way through a batch, and
    # the batch is sent again.
    chosen = "candidate.kvf" if approved else "base.kvf"
    held = ["base.kvf", "candidate.kvf"]
    batch = sets["live"][:BATCH]
    whole = bench.exchange("recover-whole", chosen, held, batch)
    cut = (whole["socket_bytes_to_worker"] + whole["socket_bytes_to_queen"]) * 2 // 5
    dropped = bench.exchange("recover-dropped", chosen, held, batch, cut_after=cut)
    again = bench.exchange("recover-again", chosen, held, batch)
    recovery = {
        "dropped_after_bytes": cut,
        "sender_reported_failure": not dropped["sender_ok"],
        "tasks_delivered_by_the_dropped_connection": dropped["completed"],
        "tasks_delivered_on_sending_again": again["completed"],
        "bytes_spent_on_the_dropped_connection": dropped["socket_bytes_to_worker"] + dropped["socket_bytes_to_queen"],
        "resumes_part_way": False,
    }

    # Rivals, a task at a time, on the same live tasks. Payload only: they
    # have no handshake, receipt or framing, so compare them with
    # `container_bytes_one_task_each`, not with socket bytes. A zlib
    # stream with a preset dictionary does not say which dictionary: both
    # sides are assumed to know. Its like is the container's tokens
    # without the header, not the whole container. zlib takes at most the
    # last 32 kB of a dictionary, so it sees under a tenth of the traces
    # the vocabulary was trained on. A trained zstd dictionary would
    # likely do better; it is not in the standard library.
    seen = b"".join(canonical(m) for part in ("train", "observe") for _, m in sets[part])
    early = b"".join(canonical(m) for _, m in sets["train"])

    def with_dictionary(data, dictionary):
        c = zlib.compressobj(9, zlib.DEFLATED, 15, 9, zlib.Z_DEFAULT_STRATEGY, dictionary[-32768:])
        return len(c.compress(data) + c.flush())

    raw = [canonical(m) for _, m in sets["live"]]
    rivals = {
        "json": sum(map(len, raw)),
        "zlib_9": sum(len(zlib.compress(r, 9)) for r in raw),
        "xz_9": sum(len(lzma.compress(r, preset=9)) for r in raw),
        "zlib_9_dictionary_from_first_traces": with_total(raw, early, with_dictionary),
        "zlib_9_dictionary_from_all_seen_traces": with_total(raw, seen, with_dictionary),
    }

    novel = [(n, m) for n, m in sets["live"] if any(x["op"] == "escalate" for x in m)]
    novel_run = bench.exchange("novel", chosen, held, novel) if novel else None

    print(json.dumps({
        "synthetic": True,
        "language_model_calls": 0,
        "seed": SEED,
        "kute_version": bench.run("--version").stdout.decode().strip(),
        "settings": {"train_tasks": TRAIN, "observe_tasks": OBSERVE, "shadow_tasks": SHADOW, "live_tasks": LIVE, "batch": BATCH, "vocab_size": VOCAB_SIZE, "top_patterns": TOP_PATTERNS, "novel_share": NOVEL_SHARE, "shadow_horizon": horizon},
        "messages_per_live_task": round(sum(len(m) for _, m in sets["live"]) / LIVE, 2),
        "observe": {"completed": sum(b["completed"] for b in observed), "tasks": OBSERVE, "escaped_symbols": sum(b["escaped_symbols"] for b in observed)},
        "patterns_proposed": len(additions),
        "vocabulary_files": {"base_bytes": (work / "base.kvf").stat().st_size, "candidate_bytes": (work / "candidate.kvf").stat().st_size},
        "shadow": {"decision": shadow["decision"], "reasons": shadow["reasons"], "comparison": shadow["comparison"]},
        "distribution": {
            "first_contact_socket_bytes": first_contact["socket_bytes_to_worker"] + first_contact["socket_bytes_to_queen"],
            "proposal_socket_bytes": None if negotiation is None else negotiation["socket_bytes_to_worker"] + negotiation["socket_bytes_to_queen"],
            "same_digest_on_both_sides": None if negotiation is None else negotiation["same_digest_on_both_sides"],
            "negotiation_ran_because": None if negotiation is None else negotiation["ran_because"],
            "signed": False,
        },
        "live": live,
        "ran_under": "candidate" if approved else "base",
        "rivals_payload_bytes_one_task_each": rivals,
        "novel_tasks": None if novel_run is None else {"tasks": len(novel), "completed": novel_run["completed"], "escaped_symbols": novel_run["escaped_symbols"]},
        "recovery": recovery,
        "llm_tokens": "not measured: no language model is called. Each agent reads the decoded message, which is byte for byte the message that was sent, so a model reading it would be given the same text whichever vocabulary carried it. Fewer bytes between agents is not fewer model tokens.",
    }, indent=1))
    return 0


def with_total(raw, dictionary, fn):
    return sum(fn(r, dictionary) for r in raw)


if __name__ == "__main__":
    sys.exit(main())
