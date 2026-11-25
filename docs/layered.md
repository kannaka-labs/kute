# Layered vocabularies: an experiment

The specification (D3) asks whether vocabularies should come in layers
(base, domain, environment, session) with local ids negotiated between
peers and a reversible fallback, and says not to replace KVF until
evidence supports it. This is that experiment. **The evidence does not
support it**, and nothing in the engine or the format changed.

Script: `benchmarks/layered_vocab.py`. Report:
`benchmarks/results/layered-vocab-code.json`. One run; the sizes are
deterministic.

## The question

Two peers hold a general vocabulary. They begin exchanging text from a
domain it was not trained on. They can train and distribute a new flat
vocabulary for the domain, or keep the one they have and agree a table
of short local ids on top of it. Which costs less?

## Setup

Real data, one domain:

* **Base**: the 32,768-token byte vocabulary trained on 90 MB of English
  Wikipedia (enwik8).
* **Domain**: Python source, the standard library of CPython 3.12.7.
  1,608 files (30.0 MB) to learn from, 178 other files (2.5 MB) to
  measure on.
* **Flat rival**: a 32,768-token vocabulary trained on the 30 MB.
* **Sessions**: the 2.5 MB cut into pieces of at least 64 kB (39
  sessions) or 4 kB (613), each encoded on its own.

A *table* is a list of base token ids in rank order. A token in the
table is sent as its rank; any other token is sent as an escape followed
by its base id. Every number is a varint, as in a KUTE container. Three
scopes:

* **domain table**: one table ranked by frequency in the 30 MB, sent
  once (39,016 bytes for 16,514 entries);
* **session table**: each session's own ranking, sent with the session
  and charged to it;
* **session over domain**: a session table of at most 127 entries, with
  the domain table underneath and the escape under that.

Every layered stream was decoded back to base ids and compared with the
original: the fallback is reversible on all of this data. Sizes below
are token bytes only, with no container header on either side.

## Result

Sessions of 64 kB. Bytes for the 2.5 MB, session tables included:

| | tokens | token bytes | after zlib -9, each session |
|---|---:|---:|---:|
| the text itself | | 2,525,866 | 549,302 |
| base vocabulary, plain ids | 869,576 | 1,535,572 | 561,302 |
| layered: domain table | 869,576 | 1,230,545 | 505,183 |
| layered: session table | 869,576 | 1,257,099 | 613,242 |
| layered: session over domain | 869,576 | 1,141,301 | 509,276 |
| flat domain vocabulary, plain ids | 596,166 | 1,114,110 | 503,222 |
| flat domain vocabulary, ids ranked by frequency | 596,166 | 904,767 | 463,406 |

Sessions of 4 kB tell the same story with one difference: session
tables cost more than they save (a full one, 1,329,947 bytes; over the
domain table, 1,110,897, level with the flat vocabulary's 1,114,186).

What it shows:

1. **Local ids recover most of the byte cost of using the wrong
   vocabulary, and none of the token cost.** A domain table takes the
   base from 1.54 MB to 1.23 MB; with a short session table on top,
   1.14 MB, within 2.5% of the flat vocabulary. The base still makes 46%
   more tokens, and no table changes that.
2. **A general compressor does the same job.** After zlib the domain
   table and the flat vocabulary are within 0.4% of each other (505,183
   and 503,222), and the session table on top makes it slightly worse.
   If the tokens are going to be compressed anyway, layering buys
   nothing measurable here.
3. **The largest gain in the table needs no layers.** Ranking a flat
   vocabulary's ids by frequency removes 19% of its token bytes
   (1,114,110 to 904,767) and 8% after zlib. A KUTE vocabulary numbers
   its tokens in the order they were made, which puts the 256 single
   bytes first; only half of those get a one-byte id. That is a possible
   change to how ids are assigned or carried, not a case for layers. It
   is not built: token ids are part of what the digest covers and what
   the tiktoken export promises.
4. **Per-session tables are expensive.** A full session table costs
   125 kB over 39 sessions and 342 kB over 613. Short ones over a shared
   table are the only form that paid for itself, and only before
   compression.

What it costs once: the flat vocabulary is 147,575 bytes in the compact
binary form (3.3 MB as JSON); the domain table is 39,016. Before
compression the flat vocabulary repays the difference after about 2.4 MB
of domain text. After zlib it takes about 140 MB.

## What this does not show

* One base, one domain, one way of cutting sessions. A domain closer to
  the base, or a much smaller one, may behave differently. Event
  vocabularies of a few hundred tokens were not tried; almost all their
  ids are one byte already.
* Nothing about what the tokens are worth to a model. A language model
  given the base vocabulary's tokens would be given 46% more of them.
  Only bytes are measured here.
* Environment scope was not separated from domain scope; there was no
  data with that structure to hand.
* "Negotiated" here means both sides derive or receive the same table.
  No negotiation protocol was built or costed beyond the table's bytes.
* The compressor is zlib, one session at a time, with no dictionary.

## Decision

KVF stays flat (ADR-0046). The child-of-a-parent relation that `kute
evolve` and `kute peer send --propose` already give is the only layering
in the engine: a proposal of 32 additions crossed the wire in 2,068
bytes in `examples/queensync` against 86,766 for the vocabulary.
