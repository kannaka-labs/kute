# KUTE Vocabulary Format (KVF) 0.1

Status: published with KUTE v0.1.0. Experimental: 0.x versions may change
incompatibly; every file states the format version it uses.

KVF is the portable form of a vocabulary: the tokens, the rules that produce
them, where they came from, and enough test data for any implementation to
check that it reads the file the same way the writer did.

The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

## 1. File

A KVF file is one JSON document, UTF-8 without a byte order mark, with the
extension `.kvf`. Whitespace and the order of object keys carry no meaning.
The reference writer puts sections in a fixed order with one token per line so
that files diff well; readers MUST NOT depend on that layout.

Readers MUST reject files over 512 MiB, and MUST validate a file (section 5)
before using any part of it.

A binary encoding of the same model is planned for a later version. KVF 0.1
defines JSON only.

## 2. Top-level object

| key               | required | content |
|-------------------|----------|---------|
| `manifest`        | yes      | identity and interpretation (2.1) |
| `tokens`          | yes      | the token table (2.2) |
| `merges`          | no       | learned merge rules (2.3) |
| `pretokenizer`    | no       | split pattern applied before merges (2.4) |
| `adapter`         | no       | how raw data was turned into tokenizer input (2.5) |
| `statistics`      | no       | integer counters from training (2.6) |
| `provenance`      | no       | corpus fingerprints, license, training configuration (2.7) |
| `test_vectors`    | no       | known inputs and their ids (2.8) |
| `representations` | no       | references to mappings from ids to other representations (2.9) |
| `integrity`       | no       | content digest and signatures (4) |

A reader MUST reject a document with other top-level keys only if it cannot
ignore them safely; the reference reader ignores unknown keys, and they are
not covered by the digest it computes. Writers MUST NOT add keys outside this
table under format version 0.1.

### 2.1 manifest

| key            | type    | meaning |
|----------------|---------|---------|
| `kvf_version`  | string  | MUST be `"0.1"` |
| `id`           | string  | vocabulary identifier (3.1) |
| `version`      | string  | semantic version 2.0.0 of this vocabulary (3.2) |
| `algorithm`    | string  | name of the algorithm that interprets the file, 1 to 64 bytes |
| `modality`     | string  | `text`, `binary`, `structured`, `events` or `other` |
| `payload_kind` | string  | `bytes` or `symbols`: what tokens are made of |
| `lossless`     | boolean | true if every input the tokenizer accepts decodes back exactly |
| `description`  | string  | optional |
| `created`      | string  | optional timestamp. Left out by default so that identical training runs produce identical files |
| `generator`    | string  | software that wrote the file, e.g. `kute 0.1.0` |

Algorithms defined by KUTE 0.1: `bpe`, `rustbpe` (same table layout as `bpe`),
`sequence-bpe`, `bytes`, `char`, `dictionary`. Other names are allowed; a
reader that does not know the algorithm can still validate, inspect and
compare the file but MUST NOT encode or decode with it.

### 2.2 tokens

An array of token objects. The token at array position `i` MUST have `id` equal
to `i`: ids are dense, start at 0, and are never reused or reordered within a
published version. At most 4,000,000 tokens.

| key         | type    | meaning |
|-------------|---------|---------|
| `id`        | integer | position in the table |
| `kind`      | string  | `byte`, `char`, `symbol`, `entry`, `merge` or `special` |
| `bytes`     | string  | the token's content as standard base64 (RFC 4648 with padding), 1 to 65,536 bytes |
| `symbols`   | array   | the token's content as 1 to 4,096 symbol names, each 1 to 1,024 bytes |
| `frequency` | integer | optional: occurrences seen in training |

Exactly one of `bytes` and `symbols` is present, and it MUST match
`manifest.payload_kind`, except that `special` tokens are exempt from that
check.

Kinds:

* `byte`: one of the 256 byte values. Byte vocabularies MUST start with all
  256, token `i` holding byte `i`.
* `char`: one Unicode character, as its UTF-8 bytes.
* `symbol`: a single named symbol.
* `entry`: a fixed dictionary entry.
* `merge`: the concatenation of two earlier tokens, defined by a merge rule.
* `special`: a reserved token. Encoding content never produces it, even when
  the content contains the same characters.

### 2.3 merges

An array of `{"left": a, "right": b, "id": c}`: where tokens `a` and `b` are
adjacent, they become token `c`.

* `a < c` and `b < c`. This makes the table acyclic and lets a reader rebuild
  it in one pass.
* Token `c` MUST have kind `merge`, and its content MUST equal the content of
  `a` followed by the content of `b`.
* Every `merge` token has exactly one rule, and no pair `(a, b)` has two.

Applying merges (algorithms `bpe`, `rustbpe`, `sequence-bpe`): within one chunk,
repeatedly find the adjacent pair with a rule whose `id` is lowest, and replace
every occurrence of that pair, left to right, with its merged token; stop when
no adjacent pair has a rule. This is the procedure tiktoken and rustbpe use.

### 2.4 pretokenizer

A regular expression, 1 to 4,096 bytes, in the syntax of the Rust
`fancy-regex` crate (the syntax tiktoken's patterns use). When present and the
input is valid UTF-8, the input is split into the pattern's successive
matches, and merges never cross a chunk boundary.

Two rules keep splitting lossless, and a conforming encoder MUST follow both:

1. Text between matches is not discarded. Each maximal unmatched run is a
   chunk of its own.
2. Input that is not valid UTF-8 is a single chunk.

Without a pretokenizer, the whole input is one chunk.

### 2.5 adapter

`{"name": string, "options": {string: string}, "lossless": boolean}`: the
adapter that produced tokenizer input from raw data during training, so later
encoding and decoding use the same one. `lossless` states whether the adapter
reproduces the original bytes exactly. It is separate from
`manifest.lossless`: a lossless tokenizer behind a canonicalizing adapter
returns canonical data, not the original bytes, and a reader can tell.

### 2.6 statistics

An object of non-negative integer counters. Names are algorithm-specific.
KUTE 0.1 writes `corpus_records`, `corpus_bytes` or `corpus_symbols`,
`merges_learned`, `requested_vocab_size`, and others. Informational only.

### 2.7 provenance

| key                   | meaning |
|-----------------------|---------|
| `corpus_fingerprints` | array of `sha256:<hex>` fingerprints of the training corpus as the tokenizer saw it (after the adapter), in order |
| `license`             | optional license of the vocabulary |
| `training`            | optional: the full training configuration, so a run can be repeated |

A vocabulary can reveal its corpus: frequent strings become tokens. Treat a
vocabulary trained on private data as private data.

### 2.8 test_vectors

An array of `{"bytes" | "symbols": input, "ids": [integers]}`, at most 1,024.
For each vector, encoding the input with this vocabulary MUST produce exactly
`ids`, and decoding `ids` MUST reproduce the input. Inputs are tokenizer
inputs (after the adapter). An implementation claims support for a KVF file
only if it passes every vector in it.

### 2.9 representations

An array of `{"mapping_id", "mapping_version", "kind", "uri"?}`. Mappings from
token ids to vectors or other representations are versioned on their own and
live outside the file; KVF only points at them. Adding or removing a reference
changes the digest but never the meaning of any token.

## 3. Identity

### 3.1 Vocabulary identifiers

Two to eight segments separated by `:`. Each segment is 1 to 64 characters
from `a-z`, `0-9`, `.`, `_`, `-`, and starts with a letter or digit. At most
255 bytes in total. Examples: `kute:industrial:events`, `language:english`.

### 3.2 Versions and immutability

`version` follows Semantic Versioning 2.0.0. A published `(id, version)` is
immutable: its content never changes. Any change to tokens, merges,
pretokenizer or adapter needs a new version. The reference writer refuses to
replace an existing file with different content unless told to.

### 3.3 Token identity

A token id is meaningful only together with the vocabulary id, version and
content digest. Equal numbers in two vocabularies are unrelated. To relate
tokens across vocabularies, compare content (the `content_hash` in the token
envelope specification).

## 4. Integrity

`integrity` is `{"algorithm": "sha256", "digest": "sha256:<hex>", "signatures": [...]}`.

The digest is SHA-256 over the canonical serialization of the document without
the `integrity` key. Canonical serialization in 0.1 is the reference
implementation's compact JSON: keys in the order of the tables in this
document, optional keys with no value left out, no insignificant whitespace,
strings escaped as `serde_json` escapes them, integers in decimal. Because
that definition leans on one implementation, a byte-level canonical form
(RFC 8785 or CBOR) is planned before 1.0; until then, treat the digest as
verifiable by the reference implementation.

A reader MUST verify the digest when present and reject the file on mismatch.

`signatures` is reserved: the field exists so signed publication can be added
without a format change, but KVF 0.1 defines no signature scheme. A digest
alone shows a file is intact, not who made it.

## 5. Validation

A reader MUST reject a file when any of these fail:

1. It parses as JSON with the shapes above; `id` and `version` are well formed.
2. `kvf_version` is one the reader supports.
3. Token ids are dense and ordered; content sizes are within limits.
4. Every merge rule satisfies 2.3.
5. Test vector ids are inside the table.
6. The digest, when present, matches.

Before encoding or decoding, the algorithm's own requirements are checked as
well (for example, the 256 byte tokens for byte vocabularies), and the
pretokenizer pattern MUST compile.

Readers MUST treat every limit and reference as hostile until checked: no
allocation sized by an unchecked number from the file, no indexing by an
unchecked id.

## 6. Interoperability

Export to tiktoken's BPE file format is defined for `bpe` and `rustbpe`
vocabularies in which no two non-special tokens have the same bytes (tiktoken
identifies a token by its bytes). The export is the non-special tokens as
`base64(bytes) id` lines, plus the pretokenizer as `pat_str` (or `[\s\S]+`
when there is none) and special tokens by name. Other vocabularies are
refused rather than exported approximately.

Import from tiktoken, Hugging Face Tokenizers and SentencePiece is not part of
KVF 0.1.

## 7. Example

```json
{
  "manifest": {
    "kvf_version": "0.1",
    "id": "kute:industrial:events",
    "version": "1.0.0",
    "algorithm": "sequence-bpe",
    "modality": "events",
    "payload_kind": "symbols",
    "lossless": true,
    "generator": "kute 0.1.0"
  },
  "adapter": { "name": "jsonl-events", "options": { "field": "tag" }, "lossless": false },
  "tokens": [
    {"id": 0, "kind": "symbol", "symbols": ["CURRENT_HIGH"]},
    {"id": 1, "kind": "symbol", "symbols": ["MOTOR_START"]},
    {"id": 2, "kind": "symbol", "symbols": ["MOTOR_TRIP"]},
    {"id": 3, "kind": "merge", "symbols": ["CURRENT_HIGH", "MOTOR_TRIP"], "frequency": 2814},
    {"id": 4, "kind": "merge", "symbols": ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"], "frequency": 2814}
  ],
  "merges": [
    {"left": 0, "right": 2, "id": 3},
    {"left": 1, "right": 3, "id": 4}
  ],
  "test_vectors": [
    {"symbols": ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP"], "ids": [4]}
  ]
}
```
