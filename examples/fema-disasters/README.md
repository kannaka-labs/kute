# Recurring sequences in FEMA disaster declarations

A public event log read with `kute.discover`: every disaster FEMA has
declared since 1953, by county and date. The question is the one
`examples/kannaka-bus` asks of a private log: do events of some kinds
follow each other more often than their counts alone would give, and do
the sequences found in the early years still occur in the later ones?

Nothing here says why a sequence recurs. The patterns are regularities
in order, and most of what is found is administrative: the same storm
declared twice.

## Run it

```
pip install kute-engine        # a release after 0.9.2: discover takes times and held-out records
python fema_fetch.py declarations.csv
python fema_patterns.py declarations.csv report.json
```

`fema_fetch.py` reads OpenFEMA's Disaster Declarations Summaries v2,
which needs no key, in pages of 10,000 rows. `fema_patterns.py` needs
the `kute` package and the standard library. The API is live and grows
daily, so a later fetch gives slightly different numbers; the report
records the SHA-256 of the file it read.

## What was measured

`report-fema-declarations.json`: fetched 2026-11-26, 70,607 rows, with
the package built from this repository after 0.9.2. The 2020 COVID-19
declarations are left out: they named every county in the country.
An event is the declaration type and the incident type together
(`DR:Hurricane`, a major disaster; `EM:Hurricane`, an emergency;
`FM:Fire`, fire management). Declarations before 2010-01-01 were
searched; those from 2010 on were held out. Discovery takes 0.13 s.

| | by county | by state |
|---|---|---|
| records searched | 3,223 counties | 58 states and territories |
| events searched | 36,654 | 3,008 |
| patterns found | 99 | 28 |
| patterns in the same records with order shuffled, 10 seeds | 22 to 26, mean 23.6 | 7 to 11, mean 8.8 |
| events held out (2010 on) | 24,808 | 2,112 |
| patterns confirmed there | 25 | 18 |

**By county** is one record a county. Its thresholds and the split were
fixed before any result was seen. The strongest patterns that held up
after 2010:

| sequence | before 2010 | median days | 2010 on | median days |
|---|---|---|---|---|
| `EM:Hurricane` then `DR:Hurricane` | 776 | 3 | 1,986 | 5 |
| `EM:Severe Ice Storm` then `DR:Severe Ice Storm` | 275 | 8 | 474 | 5 |
| `DR:Hurricane` then `EM:Hurricane` | 996 | 360 | 1,071 | 354 |
| `FM:Fire` then `FM:Fire` | 202 | 105 | 403 | 67 |

The first two are one storm declared twice: an emergency, then a major
disaster a few days later. The third is one hurricane season and the
next. Three quarters of the patterns found before 2010 were not
confirmed after it; several of the strongest never occurred again at
all (`EM:Snowstorm` then `DR:Snowstorm`, 388 times before 2010 and 0
after, which is more likely a change in how FEMA declares snow than a
change in snow).

**The county counts are not independent.** One hurricane names hundreds
of counties, and each of them gets the same two events in the same
order, so 776 is nearer to "a few dozen storms" than to 776 separate
observations. The shuffled control has the same weakness. That is why
there is a second reading.

**By state** is one record a state and one event a disaster, however
many counties it named. It was added after the county result was seen,
with a lower threshold (20) because there are a twelfth as many events.
What it finds is plainer: runs of the same kind. `FM:Fire` after
`FM:Fire` 639 times in 26 states, a median of 4 days apart, and 646
times after 2010; floods after floods a year apart; severe storms after
severe storms. 18 of 28 held up. Shuffling still leaves about 9,
because a state whose declarations are mostly fires has fire after fire
in any order: the control measures order, and a run of one symbol is
partly a count.

## What this does and does not show

* On a real public log of 61,698 events, the order carries structure
  that counts alone do not give (99 patterns against 24, and 28 against
  9), and the held-out years separate what lasted from what did not.
* It is one dataset and one run of each reading. The control was run
  with ten seeds; nothing else was repeated, and nothing was compared
  with another sequence miner.
* "Confirmed" is `kute.discover`'s own test on the held-out records, and
  it passes a pattern seen twice (`DR:Typhoon` then `DR:Typhoon` by
  state). Read the held-out counts, not the word.
* No claim is made about disasters. Declarations are decisions by an
  agency, and their rules changed over seventy years.

FEMA and the Federal Government cannot vouch for the data or analyses
derived from these data after the data have been retrieved from the
agency's website.
