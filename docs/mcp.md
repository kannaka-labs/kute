# The MCP server

`kute mcp` serves five tools over the Model Context Protocol, on standard
input and output, one JSON-RPC 2.0 message per line (the protocol's stdio
transport, revision 2025-06-18).

    kute mcp --vocab events.kvf --vocab text.kvf

A client configuration entry looks like this:

    { "command": "kute", "args": ["mcp", "--vocab", "/path/to/events.kvf"] }

Stability: **experimental**. Tool names, arguments and results may change
before 1.0.

## What it can and cannot do

The tools work on the vocabularies named with `--vocab` and on data passed
in each call. A tool call reads no file, writes no file and opens no
connection. Nothing is trained, evolved, stored between calls or published;
those operations are not offered here at all. To add a vocabulary, restart
the server with it.

## Tools

The authoritative schemas are what `tools/list` returns
(`kute_cli::mcp::Server::tools`). In short:

| tool | arguments | result |
|---|---|---|
| `inspect` | none | held vocabularies (id, version, digest, algorithm, tokens) and the limits |
| `inspect` | `vocabulary`, optional `token` or `longest` | summary, one token's record, or the longest learned tokens |
| `encode` | `vocabulary`, `text` or `data_base64`, optional `unknown` (`strict`, `escape`) | `tokens`, `ids`, `escaped_symbols`, `container_base64` |
| `decode` | `container_base64` | `text`, or `data_base64` when the bytes are not UTF-8 |
| `compare` | `a`, `b` | shared and distinct token payloads |
| `discover` | `records` (lists of symbols) or `lines`, optional `min_support`, `max_length`, `window`, `min_cohesion_bits`, `max_patterns` | recurring sequences and gapped pairs with support and cohesion |

A vocabulary is named by its digest, by its id when one version is held, or
by `id version`. `decode` takes the vocabulary from the container; it must
be one the server holds, and the digest in the container decides.

`discover` describes how often sequences of symbols recur in the records it
is given. It makes no claim about what they mean.

## Limits

| option | default | bounds |
|---|---:|---|
| `--max-input-bytes` | 1 MiB | text or data in one `encode`; symbols in one `discover` |
| `--max-output-bytes` | 4 MiB | bytes one `decode` returns, and tokens it will read |
| request line | twice the input limit plus 1 MiB | a longer line gets an error and ends the session |

`encode` lists at most 4,096 ids (`ids_omitted` says when it did not); the
container always holds all of them. `discover` reports at most 200 patterns
of each kind and gives up, with an error, past 200,000 candidate sequences.
`inspect` shows at most 100 longest tokens. `inspect` without arguments
reports the limits in force.

## Errors

A tool that cannot do what was asked returns a result with `isError: true`
and the reason as text: an unknown vocabulary, input over a limit, a
container that is damaged or names a vocabulary not held, a symbol the
vocabulary lacks under `strict`. The session continues.

Protocol errors use JSON-RPC codes and also leave the session open:

| code | when |
|---:|---|
| -32700 | the line is not JSON |
| -32600 | not a single request object (batches are not supported), or the line is over the limit (this one ends the session) |
| -32601 | a method other than `initialize`, `ping`, `tools/list`, `tools/call` |
| -32602 | `tools/call` without a name, or naming a tool that does not exist |

The process exits with status 0 when its input closes, and non-zero after
an oversized line.
