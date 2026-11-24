# KUTE token envelope and token sequence container 0.1

Status: published with KUTE v0.1.0. Experimental.

Two small formats for tokens that leave their vocabulary file:

* the **token envelope**, one token with everything needed to interpret it;
* the **token sequence container** (`.kute`), a run of token ids bound to the
  vocabulary that produced them.

Both exist for one reason: a bare number is not a token. `1048576` means
nothing until you know which vocabulary, which version, and which content.

## 1. Token envelope

A JSON object describing one token.

| key                  | type    | meaning |
|----------------------|---------|---------|
| `token_id`           | integer | id within the vocabulary |
| `vocabulary_id`      | string  | vocabulary identifier (KVF 3.1) |
| `vocabulary_version` | string  | semantic version of the vocabulary |
| `content_hash`       | string  | `sha256:<hex>` of the token's content (1.1) |
| `symbol_type`        | string  | the token's kind: `byte`, `char`, `symbol`, `entry`, `merge`, `special` |
| `modality`           | string  | modality of the vocabulary |
| `payload_encoding`   | string  | `utf8`, `base64` or `json` (1.2) |
| `payload`            | varies  | the content, in that encoding |
| `frequency`          | integer | optional: occurrences seen in training |
| `metadata`           | object  | `reversible`, `algorithm`, `length`, and `merge: [left, right]` for learned tokens |

Example:

```json
{
  "token_id": 4,
  "vocabulary_id": "kute:industrial:events",
  "vocabulary_version": "1.0.0",
  "content_hash": "sha256:5f0c...",
  "symbol_type": "merge",
  "modality": "events",
  "payload_encoding": "json",
  "payload": ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"],
  "frequency": 2814,
  "metadata": {"algorithm": "sequence-bpe", "length": 3, "merge": [1, 3], "reversible": true}
}
```

### 1.1 Content hash

The content hash identifies what a token stands for, independent of its id, so
tokens can be matched across vocabularies.

* Byte content: SHA-256 of the byte `0x62` (`b`) followed by the content bytes.
* Symbol content: SHA-256 of the byte `0x73` (`s`) followed by, for each
  symbol in order, its length in bytes as a 64-bit little-endian integer and
  then its UTF-8 bytes.

Two tokens with the same content hash have the same content. That is payload
equivalence, the only kind of cross-vocabulary equivalence KUTE 0.1 asserts.
It says nothing about meaning.

### 1.2 Payload encoding

* `utf8`: byte content that is valid UTF-8, given as a JSON string.
* `base64`: byte content that is not valid UTF-8, as standard base64.
* `json`: symbol content, as a JSON array of strings.

## 2. Token sequence container (`.kute`)

```
offset  size  content
0       5     magic: 4B 55 54 45 01   ("KUTE", format 1)
5       4     header length H, unsigned 32-bit little-endian, at most 65,536
9       H     header: one JSON object, UTF-8
9+H     ...   token ids, each an unsigned LEB128 varint of at most 5 bytes
```

Header:

| key                  | type    | meaning |
|----------------------|---------|---------|
| `vocabulary_id`      | string  | vocabulary that produced the ids |
| `vocabulary_version` | string  | its version |
| `vocabulary_digest`  | string  | its content digest (KVF 4) |
| `modality`           | string  | modality of the encoded input |
| `adapter`            | string  | optional: adapter the input went through |
| `count`              | integer | number of token ids that follow |

Rules for readers:

* Reject a file whose magic, header length or header does not parse.
* Reject a file where `count` exceeds the number of bytes after the header
  (each id takes at least one byte), before allocating anything by `count`.
* Reject an id that does not fit in 32 bits, a truncated id, and any bytes
  left after `count` ids.
* Decode only with a vocabulary whose id, version and digest all match the
  header. A vocabulary with the same id and version but a different digest is
  a different vocabulary.

Size: ids below 128 take one byte, below 16,384 two, below 2,097,152 three.
The header is typically 150 to 250 bytes, which matters for short inputs;
benchmarks report container size including it.

The container holds ids only. It is not compressed and not encrypted, and it
is not a privacy mechanism: anyone with the vocabulary can decode it.
