# KUTE token envelope and token sequence container 0.1

Status: published with KUTE v0.1.0; container format 2 (section 2.1) added in
v0.2.0; escaped symbols (section 2.2) in v0.3.0. Experimental.

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

Each id is written in its shortest form. Readers MUST refuse a padded
encoding (for example `85 00` for 5), so that equal sequences have equal
bytes. Readers before KUTE v0.4.0 accepted padded forms; no KUTE writer
has produced them.

Header:

| key                  | type    | meaning |
|----------------------|---------|---------|
| `vocabulary_id`      | string  | vocabulary that produced the ids |
| `vocabulary_version` | string  | its version |
| `vocabulary_digest`  | string  | its content digest (KVF 4) |
| `modality`           | string  | modality of the encoded input |
| `adapter`            | string  | optional: adapter the input went through |
| `count`              | integer | number of token ids that follow |
| `escapes`            | array   | optional: symbols carried literally (section 2.2) |

Rules for readers:

* Reject a file whose magic, header length or header does not parse.
* Reject a file where `count` exceeds the number of bytes after the header
  (each id takes at least one byte), before allocating anything by `count`.
* Reject an id that does not fit in 32 bits, a truncated id, and any bytes
  left after `count` ids.
* Decode only with a vocabulary whose id, version and digest all match the
  header. A vocabulary with the same id and version but a different digest is
  a different vocabulary.

The header may be up to 4 MiB; without an escape table it is a few hundred
bytes.

Size: ids below 128 take one byte, below 16,384 two, below 2,097,152 three.
The header is typically 150 to 250 bytes, which matters for short inputs;
benchmarks report container size including it.

### 2.1 Format 2: compressed ids

Added in KUTE v0.2.0. Readers accept both formats; writers choose.

```
offset  size  content
0       5     magic: 4B 55 54 45 02   ("KUTE", format 2)
5       4     header length H, as in format 1
9       H     header: the format 1 header plus `id_bytes`
9+H     ...   one zlib stream (RFC 1950) to the end of the file
```

`id_bytes` is an integer from 1 to 4: the width W of every id. Writers use
the smallest width that holds the largest id in the sequence. The zlib stream
decompresses to exactly `count` times W bytes: the ids in order, each an
unsigned little-endian integer of W bytes.

Rules for readers, on top of those above:

* Reject a format 2 file without `id_bytes`, or with a value outside 1 to 4,
  and a format 1 file that has it.
* Before decompressing, reject a file where `count` times W exceeds 1040
  times the size of the compressed body. DEFLATE cannot expand further than
  about 1032 to 1, so such a header cannot be honest.
* Never produce more than `count` times W bytes. Reject a stream that holds
  more or fewer, a stream whose checksum fails, and any bytes after it.

Fixed-width ids are used here, not varints, because the compressor finds
repeated token runs more easily when every id has the same length.

Neither format is encrypted, and neither is a privacy mechanism: anyone with
the vocabulary can decode it.

### 2.2 Escaped symbols

A vocabulary is fixed when it is published, and symbol streams keep
producing names it has never seen. An encoder has two policies:

* **strict**: refuse the input. This is the default.
* **escape**: carry each unknown symbol literally. The header gains an
  `escapes` array of strings. With a vocabulary of N tokens, the id N + i
  stands for `escapes[i]`. Each distinct unknown symbol appears once in the
  array, in order of first use. Merges are applied to the known runs on each
  side of an escaped symbol and never across it.

The vocabulary, its version and its digest are unchanged by escaping: the
sequence still names the vocabulary it was encoded with, and a reader with
that vocabulary decodes the input exactly.

Rules for readers:

* Reject a header with more than 65,536 escaped symbols, or with an escaped
  symbol that is empty or longer than 1,024 bytes.
* Reject an id of N + i where i is not an index into `escapes`.
* Files without `escapes` are read exactly as before; every 0.1 file remains
  valid.

Escaping is defined for symbol vocabularies. Byte vocabularies contain all
256 byte values and cannot meet an unknown symbol. An algorithm with no
escape support must report that instead of approximating.

### 2.3 Decode budget

A token id of a few bytes can stand for a token of up to 64 KiB, so a small
file can decode to a very large output. A reader that handles sequences from
an untrusted source should sum the lengths of the tokens named by the ids,
which needs only the token table, and refuse the sequence when the total
exceeds its budget, before producing any output. The reference
implementation exposes this as `decode_raw_bounded` and `decode --max-units`.
