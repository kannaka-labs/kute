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
| Patterns | a corpus you paste or pick, read as one event a line or as JSON Lines with an event field and an optional entity field; recurring sequences with support, cohesion and evidence | `POST /v1/discover` |
| Reports | a report written by `kute shadow`, `kute predict`, `kute live` or a benchmark script, shown as it is | a file you choose; nothing is sent |
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
| pattern timing | Not here: the service's discover operation takes no times. |
| vocabulary/merge inspection | Vocabulary. |
| evolution comparison | Compare (two vocabularies on one input); Reports for a `kute shadow` report. The page cannot evolve a vocabulary. |
| benchmark replay | Reports shows a report made earlier. It runs nothing. |
| peer status | Not here. |
| representation experiments | Not here: the service has no operation for them. One measurement exists (`docs/representations.md`). |

The Reports view refuses a file over 32 MB.

## What it does not show

Peer status, timing between events, running a benchmark again,
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

The page itself was driven once in a headless browser against a live
service through every view: each rendered the service's real answers, a
corpus line containing a script element stayed text, a request to an
outside site was blocked, and an inline script did not run. That run is
not part of the automated tests, so a later change to the script is not
covered by it.

Not checked: screen readers, browsers other than one Chromium build,
and very large answers.
