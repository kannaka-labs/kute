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
operations are not offered. There is no `/v1/train`. The one exception
is asked for by the operator and off otherwise: training jobs, below.

**Unless it is given certificates, there is no TLS and no
authentication.** For that reason it listens on the loopback interface,
and refuses to start on any other address unless `--insecure-remote` is
given. With that flag, anyone who can reach the address can use it. To
put it on a network, give it certificates, as below.

### Over TLS (after 0.9.4)

```
kute serve --vocab file.kvf --listen 0.0.0.0:8787 \
    --tls-cert service.pem --tls-key service.key --tls-client-ca callers-ca.pem
kute fetch --from https://service.example:8787 --digest sha256:... --output file.kvf \
    --tls-cert me.pem --tls-key me.key --tls-ca services-ca.pem
```

With `--tls-cert`, `--tls-key` and `--tls-client-ca`, every connection
is TLS 1.3 with a certificate on both sides, the way `kute peer` does
it, and an address that is not loopback needs no `--insecure-remote`.

* A caller is answered only if it shows a certificate issued by an
  authority in `--tls-client-ca`. Anyone else gets no HTTP at all: the
  connection is closed in the handshake.
* `--allow sha256:...` (repeatable, the fingerprint `kute peer
  fingerprint` prints) narrows that to the certificates listed. A
  caller with a certificate from the authority that is not listed is
  answered `403` `not_allowed`, whatever it asked. Without `--allow`,
  the authority is the whole list: everyone it issued a certificate to
  can do everything the service does.
* Every caller that is answered may do the same things. There are no
  roles: one that can fetch can also give a job, if jobs are on.
* The callers are `kute fetch`, `kute job` and a registry's
  `--upstream`, each with `--tls-cert`, `--tls-key` and `--tls-ca`. An
  `https://` address without them is refused before any connection.
  The service's certificate must be issued by an authority in
  `--tls-ca` and carry the host that is in the address, name or IP.
  The system's own list of authorities is never used.
* `--jobs-dir` on an address that is not loopback needs `--allow`.
  Reading by digest may be for everyone an authority knows; training
  costs the machine minutes, memory and disk, and who may ask for it is
  named.
* **Whoever can reach the port can stop the service answering, for as
  long as they like, with no certificate and almost no traffic.** A
  connection that sends nothing holds a worker until the handshake's
  time is up (`--tls-handshake-timeout`, 3 seconds, all of it counted),
  and a caller that opens another whenever one is closed keeps every
  worker held. A review measured it at a 4 second limit with 4 workers:
  8 such connections kept open made a real caller wait 4 to 7 seconds,
  and 24 left it 0 answers of 30. A plain service has the same
  weakness; TLS is what puts the service where strangers can reach it.
  The shorter time for the handshake shortens the waits and does not
  change the kind. What limits who can reach the port is a firewall,
  not this program.
* A caller whose certificate is not accepted is usually told the
  service's reason (the TLS alert). Not always: the alert can be lost
  when the connection is reset, and the caller then sees a reset, with
  a sentence saying a refused certificate can look like that.
* When every worker is busy and the line is full, a new connection is
  closed without the `503` a plain service sends: there is nothing to
  say it in before a handshake.
* What is not there: revocation other than taking a fingerprint off
  `--allow` or changing the authority, and restarting; reloading
  certificates without a restart; a browser. Studio is not served over
  TLS, since a browser has no certificate to show; `--studio` with the
  TLS options stops the start. `--peer-status` stays loopback only.
* What TLS does not change: a fetched file is still believed because of
  its digest and its signatures, not because of who sent it.

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
for, and without it these paths answer 404. With `--jobs-dir` as well
(after 0.9.4) it gives what its jobs trained too, as soon as a job is
done: see "Training jobs".

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
  refuse or stall. That is plain `http://`; for `https://` see "Over
  TLS" above.
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
  address that is not loopback. That needs the TLS options, or
  `--insecure-remote`, which means what it says: no TLS and no
  authentication, and anyone who can reach it can take every file it
  holds. Use `--registry-only` there.
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
  its digest. There is no discovery of registries and no agreement
  between registries about anything. A registry does not mirror
  another; a client can, since 0.9.4 (below).
* Upstream connections are plain HTTP, as every other connection here.

### A copy of everything a registry lists (0.9.4)

```
kute fetch --from http://registry:8787 --mirror DIR [--trusted-key BASE64]
```

asks each registry what it holds and brings every vocabulary listed
into `DIR`, as `<64 hex digits of the digest>.kvf`. Each file is fetched
by digest and checked exactly as a single fetch is: the digest computed
from the content, the file's own test vectors, and with `--trusted-key`
a signature by one of those keys. Run again, it asks only for what is
not there, and a file that is there is checked the same way before it
is counted.

* The listing is the registries' word and is used only to know what to
  ask for. A name in it that is not a digest is counted and never asked
  for; a registry can leave out what it holds, and nothing can show that.
* Nothing is deleted and nothing is replaced. A file no registry lists
  any more stays. A file under a digest's name that is not that
  vocabulary is left as it is and reported, and `--force` is refused.
  The one replacement: with `--trusted-key`, a file with the right
  content and no trusted signature is fetched again, since signatures
  are not part of the digest.
* One run takes at most `--max-files` (1024) vocabularies and stops
  asking once `--max-total-bytes` (1 GiB) have been written; each file
  is at most `--max-bytes`. What was not tried is counted, and the next
  run goes on from there.
* The report is printed either way. The command ends with an error
  unless every vocabulary listed is in the directory afterwards and
  every registry answered.
* It is a copy made once, by hand or by a scheduler. Nothing watches
  the registry.

`kute serve --vocab-dir DIR` (0.9.4) holds every `.kvf` and
`.kvfb` file in a directory, as if each had been named with `--vocab`,
so `--vocab-dir DIR --registry-only` gives a mirror on. The same serves
what `--jobs-dir` trained. The directory is read once, at start, and
not the directories under it: a file added later is held after a
restart. It is read strictly, because the operator named it: a file
with one of those endings that is not a vocabulary stops the start, and
so does a file named for a digest that its content does not have. Other
files are not looked at, and a directory with no vocabulary in it is
not a mistake: the service starts and holds none from it. Nor is a
directory that does not exist yet when it is also the `--jobs-dir`,
which the service makes. At most 4,096 files are read from one
directory, each held whole in memory.

## Training jobs (0.9.3)

`kute serve --jobs-dir DIR` takes training jobs. It is off unless the
directory is named, and refused on any address but loopback unless the
service is over TLS: without it the service has no authentication, so
whoever can reach it could make it train. Over TLS, whoever is answered
can.

```
POST /v1/jobs
{"texts": ["...the corpus, one string a document..."],
 "vocabulary_id": "example:text", "vocab_size": 8192}
```

Also taken: `version` (default `0.1.0`), `algorithm` (`bpe`), `adapter`
(`text`), `adapter_options` (an object of strings), `min_frequency`. Any
other field is refused. The answer is `202` and the job:

```
{"id": "job-1", "state": "waiting", "asked": {...}, "submitted_unix": ...,
 "started_unix": null, "finished_unix": null, "result": null, "error": null}
```

| request | answer |
|---|---|
| `GET /v1/jobs` | every job remembered, and the limits |
| `GET /v1/jobs/job-1` | that job; `state` is `waiting`, `running`, `done`, `failed` or, since 0.9.4, `withdrawn` |
| `DELETE /v1/jobs/job-1` | since 0.9.4: the job, withdrawn, if it was waiting; `409 not_waiting` otherwise |
| `GET /v1/jobs/job-1/vocabulary` | the vocabulary file of a job that is done; `409 not_ready` otherwise |

A job that is done has `result`: the `digest`, the `file` name, its
size, the summary `kute inspect` gives, and the seconds training took.
The file is `DIR/<64 hex digits of the digest>.kvf`. The same corpus and
settings give the vocabulary `kute train` gives, with the same digest.

From the command line, since 0.9.4:

```
kute job --at http://127.0.0.1:8080 --input corpus.txt     --id example:text --vocab-size 8192 --output example.kvf
```

`kute job` asks the service what it takes, gives the job, waits for it
(`--wait`, 600 seconds unless given) and writes the vocabulary. Before
anything is written the file is read as a vocabulary, its digest is
computed from its content and must be the one the service named, its
name and version must be the ones asked for, and its own test vectors
must pass.
What that does not show is that the service trained on the corpus it
was sent: that is the service's word, and `kute train` on the same
input gives the digest to compare. When the wait runs out, a job that
has not started is withdrawn; one that has started is left to end on
the service, and the error says where to ask about it. Each input file
is one document and must be UTF-8, because a corpus travels as JSON
strings; all of them go in one request, so a corpus larger than the
service reads is refused before it is sent.

What it does not do:

* **It reads no file a caller names and writes nowhere a caller names.**
  The corpus is in the request; the file's name is the digest of what
  was trained.
* **The service does not use what it trained.** The vocabulary is not
  added to what `encode` and the other operations can name, and the
  registry does not give it. The operator takes the file and starts a
  service with it, or does not.
* **A job that has started cannot be stopped, and none can be
  changed.** What a job may cost is bounded when it is taken, not while
  it runs. A job that is still waiting can be withdrawn, since 0.9.4:
  `DELETE /v1/jobs/{id}` takes it out of the line, lets its corpus go
  and answers with the job, whose state is now `withdrawn`. It stays in
  the list as what happened. For a job that is running, done, failed or
  already withdrawn the answer is `409` (`not_waiting`), and nothing
  changes.
* Jobs are not kept across a restart. The files are.
* With `--registry`, a vocabulary a job made is listed under
  `/v1/vocabularies` and given by its digest as soon as the job is done
  (after 0.9.4), with `trained_by` naming the job, and for as long as
  the service runs: not only while the job is among the 64 remembered.
  (Up to 4,096 such vocabularies; past that the oldest is no longer
  given.) At this service that adds no reader: whoever can ask the
  registry could already read the file under the job, though now for
  longer. **Further on it does:** a registry that names this one as an
  `--upstream` gives the trained file to its own callers. The listing
  does not look at the files, so one removed or changed in the
  directory is still listed; asked for, it is read and its digest
  computed, and the answer is `404`, never other content under the old
  name. After a restart the service has forgotten what its jobs made,
  and gives those files only if started with `--vocab-dir`.
* The operations are another matter: `/v1/encode` and the rest know
  only the vocabularies the service was started with. A trained
  vocabulary is used by a service after a restart with `--vocab-dir`,
  or by fetching the file.

What bounds it: one job runs at a time; `--jobs-queue` (4) may wait, and
past that a job is answered `503`; each waiting job holds its corpus in
memory, and a corpus is at most one request body; `--jobs-max-vocab-size`
(65,536) is the largest vocabulary. The last 64 finished jobs are
remembered. Nothing bounds how long one job runs except those sizes.

`--jobs-max-dir-bytes` (1,073,741,824) bounds the directory: every file
in it counts, whoever put it there. At or past the limit a job is
answered `507 no_room`, and a job taken earlier that would write past it
fails instead of writing; after that, jobs are answered `507` while
less room is left than that vocabulary needed, so none trains only to
fail. That can refuse a smaller job that would have fit. A vocabulary
larger than the whole limit is not remembered: it fails, and the jobs
after it are taken. A restart forgets what did not fit. Nothing removes files; the operator makes room. `GET /v1/jobs` gives `directory`: the files, their bytes, the
limit, `did_not_fit_bytes` (0 when nothing has failed to fit) and
`taking_jobs`, which is false whenever a job given now would be `507`. Two services sharing one directory each check it and can pass
the limit together by one file each.

A job the trainer would refuse for certain is refused when given, with
`422`: an algorithm or adapter the service does not have, `dictionary`
(it needs entries a job cannot give), `vocab_size` 0 or under 256 for
`bpe`, `min_frequency` 0. Other settings can still fail when the job
runs, such as a vocabulary too small for the symbols of a
`sequence-bpe` corpus.

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
| 409 | `not_ready` | the vocabulary of a job that is not done |
| 422 | `refused` | the operation refused the arguments; the message says why |
| 431 | `headers_too_large` | request line and headers over 16 KiB |
| 507 | `no_room` | the jobs directory holds as much as it may |
| 503 | `busy` | every worker is busy and the queue is full, or as many jobs wait as may |

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
