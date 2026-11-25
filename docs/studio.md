# KUTE Studio

A page for looking at what a running service holds and does.

    kute serve --studio --vocab parent.kvf --vocab child.kvf

and open the address it prints (`http://127.0.0.1:8787/studio`).

Stability: **experimental**. This is a first workbench: it covers what
the HTTP service can answer and says plainly what it cannot.

## What it shows

Every figure is an answer from the service that served the page, or is
read from a file you choose. Under each result is the request that
produced it, the software version, when it was asked, and the answer as
the service gave it.

| view | what it is | comes from |
|---|---|---|
| Runtime | version, operations, vocabularies held with digests, limits in force | `GET /v1/capabilities` |
| Vocabulary | summary, training statistics and corpus fingerprints, longest learned tokens, one token and the ids it was merged from | `POST /v1/inspect` |
| Encode | tokens, ids, escaped symbols, container bytes against input bytes, and whether decoding gives the text back | `POST /v1/encode`, `/v1/decode` |
| Compare | tokens shared and not shared by two held vocabularies | `POST /v1/compare` |
| Patterns | a corpus you paste or pick, read as one event a line or as JSON Lines with an event field and optional entity and time fields; recurring sequences with support, cohesion and evidence | `POST /v1/discover` |
| Peer | what a running `kute peer serve` says it is doing, refreshed every two seconds; needs `--peer-status` | `GET /v1/peer`, which reads the status file the peer keeps |
| Reports | a report written by `kute shadow`, `kute predict`, `kute live`, `kute peer serve` or `kute peer send` (one, or several appended to one file), or a benchmark script, shown as it is | a file you choose; nothing is sent |
| Not here | what the specification lists for Studio that has no backend | nothing |

Each view states its limits where they apply: a container larger than
its input, a list cut at the service's bound, lines that were left out,
an adapter that does not give back the original bytes, and that support
and cohesion describe the corpus given and were not checked on held-out
data.

## Against the specification

The specification (F2) asks for "a functional workbench over the
existing runtime: corpus import, pattern evidence and timing,
vocabulary/merge inspection, evolution comparison, benchmark replay,
peer status and representation experiments", each view consuming actual
backend output. Item by item:

| asked for | here |
|---|---|
| corpus import | Encode, Compare and Patterns take text or a file, up to the service's input limit. Nothing is stored. |
| pattern evidence | Patterns. |
| pattern timing | Patterns, when a time field is named: for each sequence, the least, 10%, median, 90% and most time from its first event to its last, in the ticks of that field. Measured on the corpus the sequences were found in; the page has no held-out timing (`kute discover --holdout` does). A line whose time is not a whole number the page can hold exactly (beyond 2^53) is left out and counted. |
| vocabulary/merge inspection | Vocabulary. |
| evolution comparison | Compare (two vocabularies on one input); Reports for a `kute shadow` report. The page cannot evolve a vocabulary. |
| benchmark replay | Reports shows a report made earlier. It runs nothing. |
| peer status | Peer (after 0.9.1): what a running `kute peer serve` says it is doing, read from the status file it keeps; see "A running peer" below. Reports reads what `kute peer serve` and `kute peer send` print when an exchange ends: who, with or without a certificate, what was delivered, which vocabularies were taken, whether a proposal was signed. A refused or failed exchange prints an error and no report; the status file records how the peer ended. |
| representation experiments | Not here: the service has no operation for them. One measurement exists (`docs/representations.md`). |

The Reports view refuses a file over 32 MB.

## A running peer

`kute peer serve` is another process, and the service does not connect
to it. The peer is asked to keep a status file, and the service is told
where it is:

    kute peer serve --listen 127.0.0.1:7411 --insecure-plain \
                    --accept-vocabularies --status-file peer.json
    kute serve --studio --vocab words.kvf --peer-status peer.json

The peer writes the file when it starts listening, when it refuses a
connection, when an exchange begins, every second while it runs, and
when it ends. The service reads it for `GET /v1/peer`, and the Peer
view asks every two seconds while it is open. It shows:

* the state: waiting for a connection, in an exchange, or ended, and
  since when;
* where it listens, whether the connection is TLS or plain, how many
  certificates are allowed, and what it takes and gives;
* in an exchange: the other side's address and certificate fingerprint,
  protocol messages handled and messages delivered so far, and the
  number of vocabularies held;
* connections refused, with the last one's address and reason;
* when ended: whether normally or with which error.

What this is, and its limits:

* It is the peer's own account, one to three seconds old. The page
  measured none of it, and nothing in it is checked against the peer.
* A file cannot show that its writer is alive. A running peer writes
  every second, so a file that says "waiting" or "in an exchange" and
  has not been written for more than three seconds is shown as not
  running. A peer that was killed is seen that way, a few seconds late.
* A peer answers one connection and ends. The view then shows "ended"
  until another peer is started with the same file.
* One file, one peer. Two peers given the same file overwrite each
  other, and the page shows whichever wrote last.
* The service reads whatever is at the path it was given: at most
  64 KiB, and only if it is a status of the kind this version writes.
  Anything else is reported as "no peer to show" and none of its
  content is passed on. Someone who can write that file can make the
  page show any status they like; it is shown as text.
* The file names who connected to the peer. `--peer-status` is refused
  on an address that is not loopback, and the file is written with the
  permissions the peer's user gives new files.
* Nothing can be sent to the peer from the page. It cannot start, stop
  or change one.
* `kute peer send` writes no status: it runs for one exchange and
  prints its report.

## What it does not show

Running a benchmark again,
representation experiments, and training or evolving a vocabulary. The
service has no operation for these, and the page shows nothing rather
than a mock-up. The Reports view shows a measurement made earlier; it
does not make it again and cannot tell whether the file was edited.

## Local-first, and what enforces it

* The page is three files compiled into the `kute` binary. It loads no
  script, style, font or image from anywhere else.
* It is served only when `--studio` is given, only on a loopback
  address (`--studio` with another address is refused, even with
  `--insecure-remote`), and only to requests that name a loopback host.
* Its `Content-Security-Policy` lets the browser run scripts and styles
  from the page's own origin only and send requests to that origin only.
  A request to another site from the page is blocked by the browser,
  and so is any script that is not the page's own file.
* Text from the service or from a file is put on the page as text. A
  corpus line holding HTML is shown as those characters.
* The page stores nothing: no cookies, no local storage. The service
  keeps nothing that is sent to it.
* A file chosen under Patterns is sent to the service on this machine
  and nowhere else; a file chosen under Reports is not sent at all.

## How it was checked

The service's tests check the headers, the host rule, that the page is
absent without `--studio`, and that the script contains none of the
calls that would put markup on the page or reach elsewhere.

The Peer view is in that test: a real `kute peer serve` waits, the test
connects to it and leaves, and the page is seen to follow from waiting
to an exchange to ended without being asked; then the file is replaced
by one with markup in its words (shown as text, and as not running) and
by one that is not a status.

The page itself is driven in a real browser by a test, since 0.9.1
(`crates/kute-cli/tests/studio_browser.rs` and the script beside it):
headless Chrome or Edge over the DevTools protocol, against a live
`kute serve --studio`, with no packages beyond Node. It goes through
every view; encodes and decodes; compares; finds a planted sequence and
reads its timing table to the figure; opens report files the command
line made (one exchange from each side, two appended, a `kute shadow`
report, an unknown file and a broken one); and checks that an event
name written as markup stays text, that a request to another origin
and a script written into the page are refused by the browser's
policy, that every request went to the page's own origin, and that no
script error occurred. Breaking the page on purpose made it fail.

It runs only when asked, because it needs Node 22 or later and a
Chromium browser:

    KUTE_STUDIO_BROWSER=require cargo test -p kute-cli --test studio_browser

Continuous integration runs it on Linux with Chrome. It was also run
here on Windows with Chrome and with Edge. Not covered: Firefox and
Safari, the file picker itself (the test hands the page a file through
the browser's debugging interface), how the page looks, and use by
keyboard or screen reader, and very large answers.
