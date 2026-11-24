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
    assert kute.__version__ == "0.2.0"
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
