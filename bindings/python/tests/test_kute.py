"""MVP criteria: Python bindings functional; compatible vocabulary export to tiktoken."""

import json
import os

import pytest

import kute
from kute import Engine, Vocabulary

TEXT = (
    "The engine reads the event log. The engine writes the event log. "
    "A receipt that names its own gaps is worth more than one that hides them. "
    "The motor started, the current rose, the motor tripped. "
    "Naïve café — 日本語のテキスト, emoji 🙂, tabs\tand\r\nWindows line ends. "
    'fn main() { println!("hello, world"); }\n'
)


@pytest.fixture(scope="module")
def engine():
    return Engine()


@pytest.fixture(scope="module")
def vocab(engine):
    return engine.train(texts=[TEXT] * 3, vocabulary_id="pytest:text", vocab_size=420)


def test_module_surface(engine):
    assert kute.__version__ == "0.9.0"
    assert kute.KVF_VERSION == "0.1"
    assert {"bpe", "bytes", "char", "dictionary", "sequence-bpe"} <= set(engine.algorithms())
    assert {"binary", "text", "json", "jsonl", "jsonl-events"} <= set(engine.adapters())


def test_text_round_trip(engine, vocab):
    for case in ["", "a", TEXT, "unseen: zyzzyva quokka", "混合 text 🙂"]:
        tokens = engine.encode(case, vocabulary=vocab)
        assert all(isinstance(t, int) for t in tokens)
        assert engine.decode(tokens, vocabulary=vocab) == case
        assert engine.decode_bytes(tokens, vocabulary=vocab) == case.encode()
    assert len(engine.encode(TEXT, vocabulary=vocab)) * 2 < len(TEXT.encode())


def test_binary_round_trip(engine):
    data = bytes(range(256)) * 8 + b"\xff\xfe\xc0\x80"
    v = engine.train(texts=[data], vocabulary_id="pytest:binary", adapter="binary", vocab_size=300)
    tokens = engine.encode(data, vocabulary=v)
    out = engine.decode(tokens, vocabulary=v)
    assert isinstance(out, bytes) and out == data


def test_paths_structured_evaluate_and_save(engine, tmp_path):
    train = tmp_path / "agent_messages.jsonl"
    held_out = tmp_path / "evaluation.jsonl"
    rows = [{"from": "kannaka", "to": f"observer-{i % 3}", "intent": "request_observation", "seq": i} for i in range(200)]
    train.write_text("\n".join(json.dumps(r) for r in rows[:150]) + "\n", encoding="utf-8")
    held_out.write_text("\n".join(json.dumps(r) for r in rows[150:]) + "\n", encoding="utf-8")

    v = engine.train(source=str(train), algorithm="bpe", adapter="jsonl", vocab_size=600, vocabulary_id="pytest:agent:messages")
    assert v.adapter == "jsonl" and v.algorithm == "bpe"
    results = engine.evaluate(vocabulary=v, dataset=str(held_out))
    assert results["round_trip_canonical_exact"] is True
    assert results["round_trip_raw_exact"] is False  # the jsonl adapter canonicalizes
    assert results["tokens"] > 0 and results["raw_bytes"] == os.path.getsize(held_out)

    path = tmp_path / "agent-vocabulary.kvf"
    v.save(str(path))
    loaded = Vocabulary.load(str(path))
    assert (loaded.id, loaded.version, loaded.digest, len(loaded)) == (v.id, v.version, v.digest, len(v))
    message = b'{"seq": 7, "from": "kannaka", "to": "observer-1", "intent": "request_observation"}'
    assert engine.encode(message, vocabulary=loaded) == engine.encode(message, vocabulary=v)
    decoded = engine.decode(engine.encode(message, vocabulary=loaded), vocabulary=loaded)
    assert json.loads(decoded) == json.loads(message)
    assert engine.verify(loaded)["failures"] == []
    with pytest.raises(ValueError):
        other = engine.train(source=str(train), adapter="jsonl", vocab_size=500, vocabulary_id="pytest:agent:messages")
        other.save(str(path))  # same path, different content: versions are immutable


def test_event_vocabulary(engine):
    log = "".join(
        f'{{"tag":"{t}"}}\n' for _ in range(30) for t in ("MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK")
    )
    v = engine.train(texts=[log], vocabulary_id="kute:industrial:events", algorithm="sequence-bpe", adapter="jsonl-events",
                     adapter_options={"field": "tag"}, vocab_size=16)
    tokens = engine.encode(log, vocabulary=v)
    assert len(tokens) < 40
    episodes = [v.token(i)["payload"] for i in range(len(v))]
    assert ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"] in episodes or ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"] in episodes
    with pytest.raises(ValueError, match="NEVER_SEEN"):
        engine.encode('{"tag":"NEVER_SEEN"}\n', vocabulary=v)


def test_errors_are_exceptions(engine, vocab, tmp_path):
    with pytest.raises(ValueError):
        engine.train(texts=["x"], vocabulary_id="Not Valid")
    with pytest.raises(ValueError):
        engine.train(texts=["x"], vocabulary_id="pytest:x", vocab_size=10)
    with pytest.raises(ValueError):
        engine.train(texts=["x"], vocabulary_id="pytest:x", algorithm="no-such-algorithm")
    with pytest.raises(TypeError):
        engine.train(vocabulary_id="pytest:x")
    with pytest.raises(TypeError):
        engine.encode(12345, vocabulary=vocab)
    with pytest.raises(ValueError):
        engine.decode([10**9], vocabulary=vocab)
    bad = tmp_path / "bad.kvf"
    bad.write_text('{"manifest": {}}', encoding="utf-8")
    with pytest.raises(ValueError):
        Vocabulary.load(str(bad))
    with pytest.raises(OSError):
        Vocabulary.load(str(tmp_path / "missing.kvf"))


def test_tiktoken_export_matches_tiktoken(engine, tmp_path):
    """The exported vocabulary, loaded by the real tiktoken, encodes identically."""
    tiktoken = pytest.importorskip("tiktoken")
    from tiktoken.load import load_tiktoken_bpe

    corpus = [TEXT * 4, "def tokenize(text):\n    return [ord(c) for c in text]\n" * 20]
    v = engine.train(texts=corpus, vocabulary_id="pytest:tiktoken", vocab_size=700, reserved_tokens=["<|endoftext|>"])
    path = tmp_path / "pytest.tiktoken"
    kwargs = v.export_tiktoken(str(path))
    assert kwargs["special_tokens"] == {"<|endoftext|>": len(v) - 1}
    enc = tiktoken.Encoding(mergeable_ranks=load_tiktoken_bpe(str(path)), **kwargs)
    assert enc.n_vocab == len(v)

    cases = corpus + [
        "",
        "a",
        "unseen words: zyzzyva quokka; numbers 1234567 and 3.14159",
        "混合 text with ελληνικά and 🙂🙃",
        "   leading, trailing and\n\n\nblank lines   ",
        "The engine tripped; the log wrote itself. " * 50,
    ]
    for case in cases:
        ours = engine.encode(case, vocabulary=v)
        theirs = enc.encode(case, disallowed_special=())
        assert ours == theirs, f"encodings differ for {case[:40]!r}"
        assert enc.decode(ours) == case
    # Every token has the same bytes on both sides.
    for token_id in range(len(v) - 1):
        assert enc.decode_single_token_bytes(token_id) == v.token_bytes(token_id)


def test_test_vectors_are_opt_in(engine):
    plain = engine.train(texts=[TEXT] * 4, vocabulary_id="test:vectors", vocab_size=300)
    assert plain.summary()["test_vectors"] == 0
    kept = engine.train(texts=[TEXT] * 4, vocabulary_id="test:vectors", vocab_size=300, embed_test_vectors=True)
    assert kept.summary()["test_vectors"] >= 1
    assert engine.verify(kept)["failures"] == []


def test_resumed_training_matches_one_run(engine):
    texts = [f"{TEXT} record {i}" for i in range(60)]
    whole = engine.train(texts=texts, vocabulary_id="test:resume", vocab_size=600)
    first = engine.train(texts=texts, vocabulary_id="test:resume", vocab_size=400)
    resumed = engine.train(texts=texts, vocabulary_id="test:resume", vocab_size=600, resume_from=first)
    assert resumed.digest == whole.digest
    with pytest.raises(Exception, match="different corpus"):
        engine.train(texts=texts[1:], vocabulary_id="test:resume", vocab_size=600, resume_from=first)


def test_discover_finds_a_planted_pattern():
    records = [["START", "HEAT", "HOLD", "DONE"] + [f"noise{i % 7}"] for i in range(40)]
    found = kute.discover(records, min_support=10)
    assert any(p["symbols"] == ["START", "HEAT", "HOLD", "DONE"] for p in found["patterns"]), found["patterns"][:3]


def test_one_vocabulary_object_is_a_persistent_session(engine, vocab, tmp_path):
    """A Vocabulary compiles its tokenizer once and cannot be changed, so many
    short messages through one object give what a fresh load gives for each."""
    path = tmp_path / "session.kvf"
    vocab.save(path)
    messages = ["", "a", "The engine reads the event log.", "unseen: zyzzyva", "混合 text 🙂", "tabs\tand\r\nlines"] * 20

    persistent = [engine.encode(m, vocabulary=vocab) for m in messages]
    reloaded = [Engine().encode(m, vocabulary=Vocabulary.load(path)) for m in messages]
    assert persistent == reloaded
    assert engine.encode_batch(messages, vocabulary=vocab) == persistent
    assert [engine.decode(t, vocabulary=vocab) for t in persistent] == messages
    assert Vocabulary.load(path).digest == vocab.digest

    # Nothing about the object can be reassigned, so its digest keeps naming
    # the vocabulary that encodes.
    for name in ["digest", "id", "version", "algorithm"]:
        with pytest.raises(AttributeError):
            setattr(vocab, name, "changed")
    assert Vocabulary.load(path).digest == vocab.digest


def test_a_changed_vocabulary_file_is_refused(vocab, tmp_path):
    path = tmp_path / "v.kvf"
    vocab.save(path)
    doc = json.loads(path.read_text(encoding="utf-8"))
    doc["manifest"]["generator"] = "someone else"
    path.write_text(json.dumps(doc), encoding="utf-8")
    with pytest.raises(ValueError, match="digest"):
        Vocabulary.load(path)


def test_events_container_keeps_whole_records():
    lines = []
    for i in range(400):
        lines.append(json.dumps({"t": 1000 + i * 3, "unit": i % 5, "event": ["START", "HEAT", "DONE"][i % 3], "note": f"n{i % 7}", "v": i / 8}))
    source = "\n".join(lines) + "\n"
    container, report = kute.events_pack(source, symbol_field="event", time_field="t", group_field="unit")
    assert isinstance(container, bytes) and container.startswith(b"KUTE-EV1\n")
    assert report["source_bytes"] == len(source.encode()) and report["container_bytes"] == len(container)
    assert report["entities"] == 5 and report["event_names"] == 3
    back = kute.events_unpack(container)
    assert [json.loads(line) for line in back.decode().splitlines()] == [json.loads(line) for line in lines]
    assert kute.events_inspect(container) == report["header"]
    # zlib is always there and gives the same records; the header says which codec.
    zlib_container, zlib_report = kute.events_pack(source.encode(), symbol_field="event", time_field="t", group_field="unit", codec="zlib")
    assert kute.events_unpack(zlib_container) == back
    assert all(c["codec"] == "zlib" for c in zlib_report["header"]["columns"])
    assert all(c["codec"] == ("zstd" if kute.ZSTD else "zlib") for c in report["header"]["columns"])
    # Refusals: a record without the event field, a limit too small, a damaged container, an unknown codec.
    with pytest.raises(ValueError):
        kute.events_pack('{"t": 1}\n', symbol_field="event")
    with pytest.raises(ValueError):
        kute.events_unpack(container, max_bytes=10)
    with pytest.raises(ValueError):
        kute.events_unpack(container[:-3])
    with pytest.raises(ValueError):
        kute.events_pack(source, symbol_field="event", codec="lzma")


def test_signals_container_keeps_its_contract():
    import math
    import struct

    values = [round(230 + 4 * math.sin(i / 40) + (i * 7919 % 13) / 50, 3) for i in range(5000)]
    values[17] = float("nan")
    values[400] = float("inf")
    values[401] = -0.0
    bits = lambda xs: [struct.pack("<d", x) for x in xs]
    exact, report = kute.signals_pack(values)
    assert exact.startswith(b"KUTE-SG1\n") and report["header"]["contract"] == "exact"
    assert bits(kute.signals_unpack(exact)) == bits(values)
    assert kute.signals_inspect(exact) == report["header"]
    bounded, report = kute.signals_pack(values, max_error=0.01)
    back = kute.signals_unpack(bounded)
    assert report["header"]["contract"] == "bounded" and report["header"]["max_error_observed"] <= 0.01
    assert len(bounded) < len(exact)
    for given, got in zip(values, back):
        if math.isfinite(given):
            assert abs(given - got) <= 0.01
        else:
            assert struct.pack("<d", given) == struct.pack("<d", got)
    assert bits(kute.signals_unpack(kute.signals_pack(values, codec="zlib")[0])) == bits(values)
    with pytest.raises(ValueError):
        kute.signals_pack(values, max_error=0)
    with pytest.raises(ValueError):
        kute.signals_unpack(exact, max_values=10)
    with pytest.raises(ValueError):
        kute.signals_unpack(exact[:-1])


def test_evolve_appends_and_keeps_every_id(engine):
    rows = [json.dumps({"tag": tag}) for _ in range(40) for tag in ["OPEN", "FLOW", "CLOSE", "OPEN", "FLOW", "ALARM"]]
    parent = engine.train(texts=["\n".join(rows) + "\n"], vocabulary_id="pytest:events", algorithm="sequence-bpe",
                          adapter="jsonl-events", adapter_options={"field": "tag"}, vocab_size=12)
    child = parent.evolve([["PURGE"], ["FLOW", "ALARM", "PURGE"]], version="0.2.0")
    assert child.version == "0.2.0" and len(child) > len(parent) and child.digest != parent.digest
    for token_id in range(len(parent)):
        assert child.token(token_id)["payload"] == parent.token(token_id)["payload"]
    new = "\n".join(json.dumps({"tag": tag}) for tag in ["OPEN", "FLOW", "ALARM", "PURGE", "CLOSE"]) + "\n"
    ids = engine.encode(new, vocabulary=child)
    back = engine.decode_bytes(ids, vocabulary=child).decode()
    assert [json.loads(line)["tag"] for line in back.splitlines()] == ["OPEN", "FLOW", "ALARM", "PURGE", "CLOSE"]
    # The added sequence is one token under the child.
    assert len(ids) < 5
    # The version must go up, and only event vocabularies evolve.
    with pytest.raises(ValueError):
        parent.evolve([["PURGE"]], version="0.1.0")
    text = engine.train(texts=[TEXT], vocabulary_id="pytest:text2", vocab_size=300)
    with pytest.raises((ValueError, NotImplementedError)):
        text.evolve([["x"]], version="0.2.0")
