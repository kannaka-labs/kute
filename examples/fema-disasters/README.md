# Recurring sequences in FEMA disaster declarations

A public event log read with `kute.discover`: every disaster FEMA has
declared since 1953, by county and date. The question is the one
`examples/kannaka-bus` asks of a private log: do events of some kinds
follow each other more often than chance would give, and do the
sequences found in the early years still occur in the later ones?

Nothing here says why a sequence recurs. The patterns are regularities
in order. The two strongest are administrative: one storm declared
twice.

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
searched; those from 2010 on were held out.

| | by county | by state |
|---|---|---|
| records searched | 3,223 counties | 58 states and territories |
| events searched (before 2010) | 36,654 | 3,008 |
| patterns found | 99 | 28 |
| with the disasters shuffled as a whole, 10 seeds | 50 to 67, mean 55.9 | 7 to 11, mean 9.1 |
| with each record shuffled by itself, 10 seeds | 22 to 26, mean 23.6 | 7 to 11, mean 8.8 |
| events held out (2010 on) | 24,808 | 2,112 |
| patterns confirmed there | 25 | 18 |

**Two controls, and by county only one of them is fair.** One hurricane
names hundreds of counties, and each gets the same events in the same
order. Shuffling each county by itself throws that duplication away, so
it sets too low a bar: 24. Shuffling the disasters as a whole (every
disaster keeps its counties and its kind, and takes another disaster's
place in time) keeps it, and gives 56. So by county the finding is 99
patterns against about 56, not against 24: more than half of what was
found is what duplication alone produces. The first version of this
example reported only the weaker control and said it shared the data's
weakness; a review showed it does the opposite, and supplied the second
control. By state, where a disaster counts once, the two controls agree.

**By county** is one record a county. The strongest patterns after 2010,
by evidence there:

| sequence | before 2010 | distinct | median days | 2010 on | distinct | median days |
|---|---|---|---|---|---|---|
| `EM:Hurricane` then `DR:Hurricane` | 776 | 28 | 3 | 1,986 | 87 | 5 |
| `EM:Severe Ice Storm` then `DR:Severe Ice Storm` | 275 | 6 | 8 | 474 | 6 | 5 |
| `DR:Hurricane` then `EM:Hurricane` | 996 | 49 | 360 | 1,071 | 66 | 354 |
| `FM:Fire` then `FM:Fire` | 202 | 190 | 105 | 403 | 392 | 67 |

The first count is occurrences in counties. "Distinct" is how many
different pairs of disasters those occurrences are: 776 counties that
saw the same two storms in the same order are one. Fire management
declarations name one county each, so there the two counts nearly
agree. Over the 78 adjacent patterns by county, 13,056 occurrences are
2,357 distinct runs of disasters.

The first two are one storm declared twice: an emergency, then a major
disaster a few days later. They are the only two of that kind among the
25 confirmed; six of the 25 have a median under 30 days, and the rest
run from 51 days to 3,967. Three
quarters of the patterns found before 2010 were not confirmed after it,
and several of the strongest never occurred again (`EM:Snowstorm` then
`DR:Snowstorm`, 388 times before 2010 and 0 after, which is more likely
a change in how FEMA declares snow than a change in snow). A pair of
symbols can be listed twice in the report, once adjacent and once with
a gap; the table uses the adjacent ones.

**By state** is one record a state and one event a disaster, however
many counties it named. It was added after the county result was seen,
with a lower threshold (20) because there are a twelfth as many events.
What it finds is plainer: 17 of the 28, and 14 of the 18 confirmed, are
runs of one kind. `FM:Fire` after `FM:Fire` 639 times in 26 states, a
median of 4 days apart, and 646 times after 2010; floods after floods a
year apart; severe storms after severe storms. Either control still
leaves about 9, because a state whose declarations are mostly fires has
fire after fire in any order: a run of one symbol is partly a count.

## What this does and does not show

* In 36,654 real events, the order carries structure beyond what
  chance arrangements give: 99 patterns against 56 by county, 28
  against 9 by state. The held-out years separate what lasted from what
  did not.
* It is one dataset and one run of each reading. Each control was run
  with ten seeds; nothing else was repeated, and nothing was compared
  with another sequence miner.
* "Confirmed" is `kute.discover`'s own test on the held-out records, and
  twice is enough to pass it. The weakest confirmed pattern here is
  `DR:Fire` then `EM:Fire` by county: seen twice after 2010, 3.4 bits of
  evidence, against 4,404 bits for the hurricane pair. Read the held-out
  counts and `held_out_evidence_bits` in the report, not the word.
* The county thresholds and the split were chosen before any result
  was seen; nothing in the repository can show that.
* No claim is made about disasters. Declarations are decisions by an
  agency, and their rules changed over seventy years.

FEMA and the Federal Government cannot vouch for the data or analyses
derived from these data after the data have been retrieved from the
agency's website.
