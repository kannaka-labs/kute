# Shadow comparison

A vocabulary with more tokens is not automatically a better one. On
HDFS, evolving a vocabulary cut the token count by 0.8% and made the
compressed output larger (`benchmarks/README.md`). `kute shadow` is the
check to run before a candidate vocabulary replaces the one in use.

    kute shadow --parent events-0.1.kvf --candidate events-0.2.kvf \
        --input held-out-1.jsonl held-out-2.jsonl --horizon 500

Stability: **experimental**.

## What it does

It encodes every input with both vocabularies, checks that each decodes
to its source, and prints a decision, `approve` or `reject`, with the
measurements and the reasons. It publishes nothing, replaces nothing and
sends nothing. The inputs should be data that neither vocabulary was
made from; the command cannot check that.

The candidate is approved when all of these hold:

* **It pays for itself.** The net saving is at least `--min-improvement`
  (default 0.01) of the parent's bytes:

      net = horizon × (parent bytes − candidate bytes) − distributions × candidate file bytes

  Bytes are compressed containers, whole. `--horizon` (default 1) is how
  much traffic the candidate will carry, as a multiple of the inputs
  given. `--distributions` (default 1) is how many times the vocabulary
  file has to be sent. The file is counted at its size on disk at the
  path given, whole. The text form of a vocabulary is many times larger
  than the compact form (`kute convert`), so give the form that will
  actually be sent; a peer that already holds the parent can be sent
  less than either, which this does not credit.
* **It is not much slower.** Encoding the inputs takes at most
  `--max-slowdown` (default 2) times as long as with the parent. Each
  is timed `--passes` times (default 3) in alternating order and the
  least time kept. It is a guard against a gross slowdown, not a
  measurement: with the same vocabulary on both sides the ratio has
  been seen as high as 1.39.
* **It escapes no more symbols** than the parent, with `--unknown escape`
  (the default). With `--unknown strict`, a candidate that cannot carry
  an input is rejected, and a parent that cannot is an error.
* **It is not the parent.**

The report also says whether the candidate records the parent as its
parent (`lineage`). That is reported, not required: a retrained
vocabulary is a legitimate candidate and has no parent.

The decision is on the total. `inputs_smaller`, `inputs_larger` and
`largest_increase_on_one_input_bytes` say how the saving is spread: a
candidate can be approved on the total while making some inputs larger.

The horizon multiplies what was saved on the inputs given, which
assumes they stand for the traffic to come. That is the assumption
drift breaks, and the reason to run the comparison again later.

`break_even_horizon` is the horizon at which the saving equals the cost
of distribution; absent when the candidate saves nothing.

Exit status is 0 whatever the decision, unless `--fail-on-reject` is
given, which makes a rejection exit 1.

## The rest of the loop

`kute shadow` is the decision step of the specification's D1 and no
more. The specification describes a loop: observe, discover, propose, encode
in shadow, compare, approve or reject, publish an immutable child,
monitor, roll back. The pieces are separate commands:

| step | command |
|---|---|
| observe, discover | `kute discover`, `kute live` |
| propose | `kute evolve` writes the child; ids of the parent keep their meaning |
| encode in shadow, compare, decide | `kute shadow` |
| publish | `kute peer`, or however files are distributed. A vocabulary is identified by its digest and is never changed in place |
| monitor | `kute shadow` again, later, on newer held-out data |
| roll back | go back to using the parent. It is still valid: containers name the vocabulary they were written with, and a child never invalidates its parent |

Nothing runs this loop automatically, and nothing approves a vocabulary
without a person or a script reading the decision.

## Not measured

Memory, task quality and predictive utility, which the specification
lists, are not measured. Timing is wall-clock on the machine it runs on.
The decision is about bytes and encoding time only.

Results on synthetic drifting data are in `benchmarks/README.md`.
