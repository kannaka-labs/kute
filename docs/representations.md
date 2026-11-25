# Composing token representations: what was measured

The specification (F1) asks for research into composing the
representations of tokens into a representation of a sequence:
order-sensitive binding, bundling, permutation, normalized composition;
an identity for each representation; portable numerical tolerances; and
measurements of retrieval, reconstruction, noise tolerance, capacity and
cost, with negative results published.

This is the first pass. **The result is negative**: on real event logs
the vectors did what exact counting does, no better, at hundreds of
times the storage. Nothing here supports a claim that these
representations carry meaning, and nothing about any medium or dynamics
that might consume them was measured.

Code: `plugins/hrm/src/compose.rs`. Experiment:
`plugins/hrm/examples/representations.rs`. Report:
`benchmarks/results/representations.json`. One run, fixed seed.

    cargo run --release -p kute-hrm --example representations -- \
        openssh openssh-events.jsonl pid  hdfs hdfs-events.jsonl block

Stability: **experimental, research**. Expect it to change or go.

## What was built

A token is the phase code already in this crate: unit phasors whose
phases come from a hash of the token's content. Two tokens are similar
only if they are the same token. Three standard ways to make one vector
from a sequence (vector symbolic architectures; none is new):

| | how | order |
|---|---|---|
| bundle | sum of the tokens | lost |
| permute-bundle | token at position p rotated by p places, then summed | kept |
| bind-chain | rotated tokens multiplied elementwise (phases add) | kept |

Every result is scaled to unit energy; similarity is the real part of
the inner product.

**Identity.** A composition is named by everything that fixes its
vectors: algorithm and its version, dimension, configuration, and the
token mapping underneath with its version. For example:

    kannaka:hrm:compose:permute-bundle@0.1.0/dimension=1024/rotation=1-per-position/base=kannaka:hrm:phase-hash@0.1.0

Vectors under different identities are not comparable, and vectors of
different dimensions are refused.

**Portable tolerance.** Arithmetic is `f32`, summed in index order;
sines and cosines come from the platform. Two implementations of one
identity agree if the similarity of any two unit-energy vectors differs
by at most `1e-4` (`PORTABLE_TOLERANCE`). A test pins three recorded
similarities and runs on Linux and Windows in CI. That is two platforms
and one compiler family, which is a start and not a proof.

## Synthetic sequences: the arithmetic works as the textbooks say

Made-up tokens drawn uniformly from 64. This measures the operations and
says nothing about any data.

* **Order.** The same six tokens reordered: bundle similarity 1.00;
  permute-bundle and bind-chain 0.00 (to two places, at every dimension).
* **Finding a constituent again.** Permute-bundle finds the token at a
  given position every time for sequences up to 64 long in 1,024
  dimensions; in 256 dimensions it falls to 97% at length 32 and 66% at
  64. Bind-chain finds it every time, but only when given every other
  constituent; given none it finds nothing. Bundle can say a token is
  present, never where.
* **Noise.** Sequence of 8 in 1,024 dimensions, noise added to the
  composed vector: permute-bundle finds the token 100% of the time with
  noise as strong as the signal, 99% at ten times the signal's energy,
  27% at a hundred times. Bind-chain (others known) 100%, 100%, 93%.
* **Cost.** About 13 microseconds to compose six tokens and 3 to compare
  two vectors at 1,024 dimensions, one thread, unoptimized; linear in
  dimension. A vector is 8 bytes a dimension: 8 kB at 1,024.

## Real event logs: no better than counting

Windows of six consecutive events of one entity, from two real logs:
OpenSSH (638,947 events, 38 distinct, entity = process; 1,915 distinct
windows, 1,041 of them a reordering of another) and HDFS (2,000,000
events, 25 distinct, entity = block; 4,288 distinct windows, 3,759 of
them a reordering of another). Two baselines use no vectors:

* **counts**: how many of each event the window has;
* **positions**: which event is at each of the six places.

**Nearest window.** Some windows are stored, each as its own vector. A
stored window has one event replaced, and the nearest stored window is
looked up. Share of 500 queries where it was the original (a tie for
nearest among k earns 1/k):

| stored | counts | bundle | positions | permute-bundle | bind-chain |
|---|---:|---:|---:|---:|---:|
| OpenSSH, 100 | 0.77 | 0.76 | 0.98 | 0.97 | 0.01 |
| HDFS, 100 | 0.65 | 0.67 | 0.98 | 0.99 | 0.00 |
| HDFS, 1,000 | 0.23 | 0.22 | 0.83 | 0.84 | 0.00 |

Vector columns are at 1,024 dimensions; 256 and 4,096 give the same
figures within two points. Reading it:

* Bundle equals counts, and permute-bundle equals positions, to within
  the noise of 500 queries. Order matters a great deal on these logs
  (0.23 against 0.83), and keeping it by rotation works. But a table of
  six small numbers keeps it as well as 1,024 complex ones.
* Bind-chain is at chance. A window with one event changed is unrelated
  to the original under it. It is a hash.

An earlier version of this table counted a tie as a miss and showed the
vectors ahead of the exact baselines. That was the scoring: exact scores
tie often, vector scores never do, and noise was being credited as
skill. With ties split the difference went away.

**Recognition from one memory.** All stored windows are summed into a
single vector, and a window is scored by its similarity to that sum.
Share of (stored, not stored) pairs ranked rightly; 0.5 is chance:

| stored | counts | bundle | positions | permute-bundle | bind-chain, 256 / 1,024 / 4,096 dimensions |
|---|---:|---:|---:|---:|---:|
| OpenSSH, 100 | 0.50 | 0.50 | 0.55 | 0.55 | 0.93 / 1.00 / 1.00 |
| HDFS, 100 | 0.56 | 0.56 | 0.63 | 0.63 | 0.92 / 1.00 / 1.00 |
| HDFS, 1,000 | 0.49 | 0.49 | 0.50 | 0.50 | 0.69 / 0.83 / 0.98 |

* The sums cannot tell a stored window from an unstored one made of the
  same common events. Neither can the exact baselines summed the same
  way, so that is the method (a sum of everything) and not the vectors.
* Bind-chain can, because it behaves as a hash of the exact window, and
  the memory as a set of hashes. Its capacity is what one would expect:
  1,000 windows need about 4,096 dimensions (32 kB) to be told apart
  98% of the time. The 1,000 windows themselves are 6 kB as bytes, and
  an ordinary set answers the question exactly.

## What this does and does not say

It says the operations are implemented correctly and behave as
documented elsewhere; that on these two logs order-sensitive
composition retrieves much better than order-free; and that none of it
beats, or differs measurably from, exact counting that costs far less.

It does not say:

* anything about learned representations. Co-occurrence statistics or
  trained embeddings, where similar tokens have similar vectors, were
  not built. They are where a vector could carry something a count does
  not, and they are the baseline the specification names that is still
  missing.
* anything about HRM or any associative or predictive medium. The
  "memory" here is addition and retrieval is an inner product. Sequence
  completion and generalization were not measured.
* that the vectors are useless for a medium that needs vectors as input.
  If something consumes them, this is a repeatable, identified way to
  make them. Whether that something then does anything useful is the
  open question, as it was before.

No claim of semantic understanding, emergence or intelligence is made
or supported.
