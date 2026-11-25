# The HTTP service

`kute serve` offers the same five read-only operations as `kute mcp`, over
HTTP, through the same code.

    kute serve --vocab events.kvf --vocab text.kvf

It prints where it listens as one JSON value and then serves until it is
stopped:

    {"api": "v1", "listening": "127.0.0.1:8787", "loopback_only": true}

Stability: **experimental**. Paths, arguments and results may change before
1.0. The interface is versioned in the path so that a change can arrive as
`/v2/` beside `/v1/`.

## What it can and cannot do

It works on the vocabularies named with `--vocab` and on data sent in each
request. A request reads no file, writes no file and opens no connection.
Nothing is trained, evolved, stored between requests or published; those
operations are not offered. There is no `/v1/train`.

**There is no TLS and no authentication.** For that reason it listens on
the loopback interface, and refuses to start on any other address unless
`--insecure-remote` is given. With that flag, anyone who can reach the
address can use it. To put it on a network, put something that does TLS
and authentication in front of it.

On loopback it also guards against web pages, which can reach a local
port from a browser: it answers only requests whose `Host` is a loopback
name (`localhost`, `127.0.0.1`, `[::1]`), it accepts a body only as
`Content-Type: application/json`, and it sends no cross-origin headers.

## Requests

| request | body | answer |
|---|---|---|
| `GET /v1/capabilities` | none | held vocabularies, the operations, the limits in force |
| `POST /v1/inspect` | `{}` or `{"vocabulary": ..., "token": n}` or `{"vocabulary": ..., "longest": n}` | as the MCP tool |
| `POST /v1/encode` | `{"vocabulary": ..., "text": ...}` or `"data_base64"`; optional `"unknown"` | `tokens`, `ids`, `escaped_symbols`, `container_base64` |
| `POST /v1/decode` | `{"container_base64": ...}` | `text`, or `data_base64` when the bytes are not UTF-8 |
| `POST /v1/compare` | `{"a": ..., "b": ...}` | shared and distinct token payloads |
| `POST /v1/discover` | `{"records": [[...], ...]}` or `{"lines": ...}` with the optional parameters | recurring sequences with their support |

The body of a POST is the arguments of the MCP tool of the same name, and
the answer is that tool's result; [mcp.md](mcp.md) describes each one, and
how a vocabulary is named.

    curl -s localhost:8787/v1/encode -H 'Content-Type: application/json' \
         -d '{"vocabulary": "example:words", "text": "hello"}'

One request per connection: every answer carries `Connection: close`.

## Errors

Every answer is JSON. A failure is `{"error": {"code": ..., "message": ...}}`
with one of these:

| status | code | when |
|---:|---|---|
| 400 | `bad_request`, `bad_json` | not HTTP/1.x, a broken header, a body that is not JSON, bytes after the body |
| 403 | `forbidden_host` | on loopback, a `Host` that is not a loopback name |
| 404 | `not_found` | a path outside `/v1/`, or an operation that does not exist |
| 405 | `method_not_allowed` | `GET` on an operation, anything but `GET` on capabilities |
| 408 | `timeout` | the request did not arrive whole in time |
| 411 | `length_required` | a POST without `Content-Length`, or with a transfer encoding |
| 413 | `too_large` | a declared body longer than the limit; refused before any of it is read |
| 415 | `unsupported_media_type` | a body not declared as `application/json` |
| 422 | `refused` | the operation refused the arguments; the message says why |
| 431 | `headers_too_large` | request line and headers over 16 KiB |
| 503 | `busy` | every worker is busy and the queue is full |

## Limits

| option | default | bounds |
|---|---:|---|
| `--max-input-bytes` | 1 MiB | text or data in one `encode`; symbols in one `discover` |
| `--max-output-bytes` | 4 MiB | bytes one `decode` returns, and tokens it will read |
| body | twice the input limit plus 1 MiB | checked against `Content-Length` before reading |
| `--workers` | 4 | requests handled at once |
| queue | 16 | accepted connections waiting for a worker; past it, 503 |
| `--timeout` | 10 s | for a request to arrive whole, and for its answer to be taken |

The operations' own limits (ids listed, patterns reported, candidates
considered) are those of the MCP server.

What is not bounded: the time an operation itself takes once its request
is read. An `encode` of 1 MiB takes milliseconds; a `discover` at the
limits takes longer, and four of them occupy every worker. There is no
per-client rate limit. An answer is written with a deadline on each
write, not on the whole, so a client that reads very slowly can hold a
worker longer than the timeout. A request refused before its body is read
(too large, wrong host) is answered and closed without reading the rest,
which some clients report as a reset instead of showing the answer.
Connections that send nothing hold a worker until
the timeout; with every worker and the queue held, new connections are
told `busy` until some time out.
