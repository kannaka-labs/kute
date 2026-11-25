# Measuring prediction

`kute predict` measures how much an entity's recent events say about
its next one, on time that was held out, against two baselines.

    kute predict --input events.jsonl --symbol-field event --time-field t --group-field block

Stability: **experimental**. Library: `kute_discover::predict`.

It is a measurement. It does not write a model, and the predictor inside
it is deliberately simple. Its use is to answer, for a stream of events:
is there structure in the order of events that would let the next one
be anticipated, how much, and does it last.

## What is compared

The stream is cut at one moment, chosen so that `--holdout-fraction`
(default 0.25) of the events come at or after it. Events before the cut
are counted. Each event after it is predicted from the earlier events of
the same entity. Three predictors, built the same way:

| name | looks at |
|---|---|
| `frequency` | nothing: only how common each event was before the cut |
| `previous` | the one event before, or the fact that this is the entity's first |
| `context` | up to `--max-context` events before (default 3), using the longest run seen at least `--min-count` times (default 5) |

Each level is smoothed toward the one below it and the lowest toward
equal shares, with one share kept for event names never seen.

For each predictor the report gives:

* `bits_per_event`: the average surprise, the mean of −log2 of the
  probability given to the event that came. Lower is better. The
  difference between two predictors is how many bits an event the
  better one saves.
* `top1_accuracy`: how often the most likely event was the one that came.
* `calibration_error`: over ten bins of stated confidence, the mean gap
  between the confidence and how often it was right. `context_calibration`
  lists the bins.

`context_over_previous` and `previous_over_frequency` are the
differences in bits, each with the middle 95% of the same difference
over resampled held-out entities.

## What it controls for

* **Overlap.** The cut is in time. Nothing counted comes after anything
  predicted.
* **Entity leakage.** `unseen_entities_only` gives the three scores
  again over entities with no event before the cut.
* **Drift.** `windows` gives the scores over the held-out time cut into
  `--windows` parts (default 4) of equal event count, in order.
* **Repeated testing.** The three predictors and every setting are fixed
  before the held-out events are read, and none is chosen by its score.
  That covers one run. Running again with other settings and reporting
  the best is repeated testing; the command cannot see it, so say how
  many settings were tried.

## What it does not show

* **Cause.** A past that predicts the next event is associated with it
  in this data. Nothing here separates that from a common cause.
* **A bound on what is predictable.** A better predictor would score
  better. The bits saved are what this one found, not all there is.
* **Mutual information as such.** The difference in bits between two
  predictors estimates how much information the longer past carries
  about the next event, through these particular predictors and with
  their errors. It is not an estimate with a guarantee in either
  direction.
* The intervals resample entities and so reflect variation between
  entities. They do not account for drift, and with one cut in time
  there is one held-out period, not many.

The whole stream is held in memory, and at most 5,000,000 distinct runs
of past events are counted.

Results on real event logs are in `benchmarks/README.md`.
