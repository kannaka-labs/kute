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
| `POST /v1/discover` | `{"records": [[...], ...]}` or `{"lines": ...}` with the optional parameters; with `records`, optionally `"times": [[...], ...]`, one whole number for each symbol (after 0.8.1) | recurring sequences with their support; with `times`, also `timing`: how long each took, in the ticks given, measured on the same records |

The body of a POST is the arguments of the MCP tool of the same name, and
the answer is that tool's result; [mcp.md](mcp.md) describes each one, and
how a vocabulary is named.

    curl -s localhost:8787/v1/encode -H 'Content-Type: application/json' \
         -d '{"vocabulary": "example:words", "text": "hello"}'

One request per connection: every answer carries `Connection: close`.

## Registry (after 0.8.1)

Started with `--registry`, the service also gives the files of the
vocabularies it was started with to whoever asks. It is off unless asked
for, and without it these paths answer 404.

| request | answer |
|---|---|
| `GET /v1/vocabularies` | what is held: id, version, digest, algorithm, tokens, and `signed_by`, the key names of the signatures each file carries, as the file states them |
| `GET /v1/vocabularies/sha256:<64 hex>` | that vocabulary's KVF file, with the signatures it was loaded with; 404 if none with that digest is held |

It is read-only: nothing can be sent to it, and any other method is
405. A file is asked for by the digest of its content and by nothing
else; a name, a version or a path is 404.

`--registry-only` makes the process a registry and nothing else: the
five operations answer 404 and `capabilities` lists none, so a caller
cannot make it compute. It cannot be combined with `--studio`.

The other side is `kute fetch`:

    kute serve --vocab signed.kvf --registry-only
    kute fetch --from http://127.0.0.1:8787 --list
    kute fetch --from http://127.0.0.1:8787 --digest sha256:... \
               --output got.kvf --trusted-key <BASE64>

`fetch` recomputes the digest from what arrives and writes the file only
if it is the digest asked for and the vocabulary passes its own test
vectors; with `--trusted-key`, only if one of those keys signed it, as
`kute verify` checks. It needs a stated `Content-Length`, takes at most
`--max-bytes` (256 MiB unless told), and gives the whole exchange
`--timeout` seconds (60). `--list` prints what the registry says it
holds and checks nothing.

What this is and is not:

* The registry and the network are not trusted with what you get: a
  registry that sends other content, or anyone who changes it on the
  way, gets nothing written. They are trusted with nothing else either,
  and that is all the protection there is: the connection is plain
  HTTP, so both can see what was asked for and what was sent, and can
  refuse or stall. `https://` addresses are refused, not built.
* The digest has to come from somewhere you trust. The listing is the
  registry's own account. A digest read from the listing and fetched
  without `--trusted-key` proves only that the registry sent what it
  said it would.
* To be reached from another machine the service must listen on an
  address that is not loopback, which needs `--insecure-remote` and
  means what that option says: no TLS and no authentication, and anyone
  who can reach it can take every file it holds. Use `--registry-only`
  there.
* No uploads, no accounts, no search, no federation between registries,
  no expiry or revocation. The registry holds the files named when it
  started and no others.

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
(too large, wrong host) is answered, and then up to 1 MiB of what the
client is still sending is read and discarded for at most half a
second before the close, so that the answer is not lost to a reset
(it was, on macOS, until this was added). A client that sends more than
that after being refused may still see a reset instead of the answer.
The same wait, at most 50 ms, is spent on a client that has its answer
and does not close.
Connections that send nothing hold a worker until
the timeout; with every worker and the queue held, new connections are
told `busy` until some time out.
