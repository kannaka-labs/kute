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
| `POST /v1/discover` | `{"records": [[...], ...]}` or `{"lines": ...}` with the optional parameters; with `records`, optionally `"times": [[...], ...]`, one whole number for each symbol (0.9) | recurring sequences with their support; with `times`, also `timing`: how long each took, in the ticks given, measured on the same records |

With `--peer-status FILE` (0.9.2, loopback only) there is one
more: `GET /v1/peer` answers with the status file a `kute peer serve
--status-file FILE` keeps on this machine: `present`, and when it is,
`status` (the file), `age_seconds`, `stale` (it claims to run and has
not been written for more than three of its beats) and `means`. The
service does not connect to the peer; it reads at most 64 KiB from that
one path, and passes on nothing that is not a status. Without the
option the path is 404. `docs/studio.md`, "A running peer".

The body of a POST is the arguments of the MCP tool of the same name, and
the answer is that tool's result; [mcp.md](mcp.md) describes each one, and
how a vocabulary is named.

    curl -s localhost:8787/v1/encode -H 'Content-Type: application/json' \
         -d '{"vocabulary": "example:words", "text": "hello"}'

One request per connection: every answer carries `Connection: close`.

## Registry (0.9)

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
`--max-bytes` (64 MiB unless told; 256 MiB in 0.9.0), and gives the
whole exchange `--timeout` seconds (60), looking the name up included
(since 0.9.1). The whole file is held and read before its digest is
known, so `--max-bytes` is what a registry can make the client hold. `--list` prints what the registry says it
holds and checks nothing.

What this is and is not:

* The registry and the network are not trusted with what you get: a
  registry that sends other content, or anyone who changes it on the
  way, gets nothing written. They are trusted with nothing else either,
  and that is all the protection there is: the connection is plain
  HTTP, so both can see what was asked for and what was sent, and can
  refuse or stall. `https://` addresses are refused, not built.
* The file is written with whatever signatures arrived in it. A
  signature is outside what the digest covers, so a registry can add
  one, and a file with the right digest can carry a signature nobody
  made honestly. With `--trusted-key` one signature from a key you
  trust has been checked; without it none has, and the report says how
  many the file carries and that they are claims until `kute verify` is
  run. They are kept and not removed because a signature you did not
  check today is the one you may check tomorrow.
* The digest has to come from somewhere you trust. The listing is the
  registry's own account. A digest read from the listing and fetched
  without `--trusted-key` proves only that the registry sent what it
  said it would.
* To be reached from another machine the service must listen on an
  address that is not loopback, which needs `--insecure-remote` and
  means what that option says: no TLS and no authentication, and anyone
  who can reach it can take every file it holds. Use `--registry-only`
  there.
* No uploads, no accounts, no search, no expiry or revocation. The
  registry holds the files named when it started and, if it was given
  upstreams, copies of what it has fetched from them (below).

### Registries that ask other registries (0.9.1)

A registry can be told of others, and a client can be told of several.

    kute serve --registry-only --vocab ours.kvf \
               --upstream http://10.0.0.5:8787 --upstream http://10.0.0.6:8787
    kute fetch --from http://a:8787 --from http://b:8787 \
               --digest sha256:... --output got.kvf --trusted-key <BASE64>

On the registry: asked for a digest it does not hold, it asks each
`--upstream` in the order given for that digest, checks what arrives as
`kute fetch` does (it parses, the digest recomputed from its content is
the one asked for, its test vectors pass), keeps it in memory and gives
it. An upstream that fails, stalls or sends something else is passed
over. With `--upstream-trusted-key`, a file is kept only if one of
those keys signed it; without, the file is passed on with whatever
signatures it came with, checked by nobody.

On the client: `--from` may be repeated. The registries are asked in
order and the first whose answer passes every check, the signature
included, is used. The report names it under `registry` and lists the
ones before it under `registries_passed_over`, each with why. If none
gives the file nothing is written and each one's reason is printed.
`--timeout` and `--max-bytes` are for each registry, so the longest wait
is their number times `--timeout`. `--list` with several prints a row
for each, a registry that cannot be reached being its row.

What bounds it:

* One registry further and no more. A registry asking another sends the
  header `Kute-Registry-Via`, and a registry that sees it answers from
  what it holds or has kept, and asks nobody. So registries that name
  each other, or themselves, cannot pass a question round, and a
  question cannot be made to travel a chain. Anyone can send that
  header; all it does is make a registry do less.
* One question upstream at a time. While a registry is waiting on an
  upstream, a second caller asking for something it does not hold is
  told 503 with `Retry-After: 1` and may try again. Files it holds, and the listing,
  are given without waiting. A caller asking for digests nobody has can
  therefore delay other callers' upstream fetches, and cannot stop the
  registry answering for what it has. One worker is occupied for up to
  the number of upstreams times `--upstream-timeout` (10 s each).
* A digest every upstream answered 404 for is remembered as absent for
  60 seconds (the last 1024 of them), and is not asked for again in
  that time. An upstream that failed is not counted as having said no.
* An upstream that failed, stalled or sent something that did not pass
  is left alone for 30 seconds, and questions go straight to the next
  one. So a slow upstream costs its timeout once in that time, not on
  every question, and a caller repeating questions cannot use it to
  keep the registry busy. While it rests, what only it holds is 404.
* `--upstream-max-bytes` (16 MiB) is the largest file taken, and
  `--upstream-cache-bytes` (64 MiB) is all that is kept; the oldest
  files go first. A file too large to keep is still given, and asked
  for again next time.
* Only the digest is sent upstream, to the addresses the operator
  named. A caller cannot make a registry connect anywhere else.

What it does not do:

* It does not make any registry more trusted. An upstream is trusted
  exactly as a registry is by a client: not for content, and for
  nothing else either. A file that came through three registries is
  checked by the client just as one that came from one.
* A registry without `--upstream-trusted-key` can be given, by an
  upstream, the right content with its signatures removed or with
  others added, and it will keep and give that copy until it restarts
  or the copy is pushed out. A client that requires a signature refuses
  such a copy and can name another registry. A registry that should
  only ever pass on signed files needs `--upstream-trusted-key`.
* The listing does not include what upstreams hold, or what has been
  fetched from them: it is what the registry was started with, and
  beside it `upstreams`, the addresses it asks. No registry repeats
  another's listing, so there is no listing to believe at second hand.
* Nothing is written to disk, nothing is refreshed, and nothing is
  announced: a registry learns of a vocabulary only by being asked for
  its digest. There is no discovery of registries, no mirroring of a
  whole registry, and no agreement between registries about anything.
* Upstream connections are plain HTTP, as every other connection here.

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
