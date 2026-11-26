"""Recurring sequences in FEMA disaster declarations (real, public data).

    python fema_patterns.py declarations.csv report.json

Two readings of the same file, both with `kute.discover`:

* by county: one record a county, its declarations in date order;
* by state: one record a state, one event a disaster, so a storm that
  named two hundred counties counts once.

In each, the declarations before SPLIT are searched and the ones from
SPLIT on are held out. There are two controls, ten seeds each:

* each record shuffled by itself: what the same counts of the same
  events give when order means nothing. By county this is too weak a
  bar: three hundred counties that shared two storms get three hundred
  different orders, so the control loses the duplication the data has.
* the disasters shuffled as a whole: every disaster keeps its counties
  and its symbol and takes another disaster's place in time, so counties
  that shared a storm still share it. This is the fair bar by county.
  (A review of this example found the first control wanting and gave
  the second.)

The county reading and its thresholds were fixed before any result was
seen. The state reading was added after, because one disaster names many
counties and the county counts are not independent.
"""
import collections, csv, datetime, hashlib, json, random, sys

import kute

SPLIT = datetime.date(2010, 1, 1)
READINGS = {
    "by_county": dict(min_support=50, max_length=4, window=4, max_patterns=200),
    "by_state": dict(min_support=20, max_length=4, window=4, max_patterns=200),
}
SEEDS = 10


def read(path):
    """(entity -> events) for each reading; an event is (day, disaster, symbol)."""
    county, state = collections.defaultdict(dict), collections.defaultdict(dict)
    rows = 0
    for r in csv.DictReader(open(path, encoding="utf-8")):
        rows += 1
        # The 2020 COVID-19 declarations named every county in the country.
        if r["incidentType"] == "Biological":
            continue
        day = datetime.date.fromisoformat(r["declarationDate"][:10]).toordinal()
        event = (day, int(r["disasterNumber"]), r["declarationType"] + ":" + r["incidentType"])
        state[r["fipsStateCode"]].setdefault(r["disasterNumber"], event)
        # County 000 is a declaration for a whole state or a tribal area.
        if r["fipsCountyCode"] != "000":
            county[r["fipsStateCode"] + r["fipsCountyCode"]].setdefault(r["disasterNumber"], event)
    return rows, {"by_county": county, "by_state": state}


def halves(entities):
    searched, held = ([], [], []), ([], [], [])
    for name in sorted(entities):
        events = sorted(entities[name].values())
        for early, (records, times, disasters) in ((True, searched), (False, held)):
            part = [e for e in events if (e[0] < SPLIT.toordinal()) == early]
            if len(part) >= 2:
                records.append([e[2] for e in part])
                times.append([e[0] for e in part])
                disasters.append([e[1] for e in part])
    return searched, held


def distinct(symbols, records, disasters):
    """How many different runs of disasters make an adjacent pattern's
    occurrences: 776 counties with the same two storms are one."""
    n, seen = len(symbols), set()
    for record, numbers in zip(records, disasters):
        for i in range(len(record) - n + 1):
            if record[i:i + n] == symbols:
                seen.add(tuple(numbers[i:i + n]))
    return len(seen)


def disasters_shuffled(entities, params, seed):
    """Patterns found when the searched disasters trade places in time."""
    rng = random.Random(seed)
    split = SPLIT.toordinal()
    early = sorted({(e[0], e[1]) for events in entities.values() for e in events.values() if e[0] < split})
    slots = early[:]
    rng.shuffle(slots)
    where = {disaster[1]: slot for disaster, slot in zip(early, slots)}
    records = []
    for name in sorted(entities):
        part = [e for e in entities[name].values() if e[0] < split]
        if len(part) >= 2:
            records.append([e[2] for e in sorted(part, key=lambda e: where[e[1]])])
    return len(kute.discover(records, **params)["patterns"])


def reading(entities, params):
    (searched, searched_t, searched_d), (held, held_t, held_d) = halves(entities)
    found = kute.discover(searched, times=searched_t, held_out=held, held_out_times=held_t, **params)
    control = []
    for seed in range(SEEDS):
        rng = random.Random(seed)
        control.append(len(kute.discover([rng.sample(r, len(r)) for r in searched], **params)["patterns"]))
    whole = [disasters_shuffled(entities, params, seed) for seed in range(SEEDS)]
    after = {p["pattern"]: p for p in found["held_out"]["patterns"]}
    days = lambda timing, i: (timing[i]["duration"] or {}).get("median")
    patterns = [{
        "symbols": p["symbols"], "kind": p["kind"], "support": p["support"], "records": p["records"],
        "cohesion_bits": round(p["cohesion_bits"], 3), "evidence_bits": round(p["evidence_bits"], 1),
        "median_days": days(found["timing"], i),
        "distinct_disaster_runs": distinct(p["symbols"], searched, searched_d) if p["kind"] == "contiguous" else None,
        "held_out_distinct_disaster_runs": distinct(p["symbols"], held, held_d) if p["kind"] == "contiguous" else None,
        "held_out_support": after[i]["support"], "held_out_confirmed": after[i]["confirmed"],
        "held_out_evidence_bits": None if after[i].get("evidence_bits") is None else round(after[i]["evidence_bits"], 1),
        "held_out_median_days": days(found["held_out_timing"], i),
    } for i, p in enumerate(found["patterns"])]
    return {
        "params": found["params"],
        "entities": len(entities), "events": sum(len(e) for e in entities.values()),
        "searched_records": len(searched), "searched_events": sum(map(len, searched)),
        "held_out_records": len(held), "held_out_events": sum(map(len, held)),
        "distinct_symbols_searched": found["observation"]["alphabet"],
        "patterns_found": len(patterns),
        "patterns_with_each_record_shuffled": control,
        "patterns_with_each_record_shuffled_mean": sum(control) / len(control),
        "patterns_with_disasters_shuffled": whole,
        "patterns_with_disasters_shuffled_mean": sum(whole) / len(whole),
        "found_that_are_runs_of_one_symbol": sum(1 for p in patterns if len(set(p["symbols"])) == 1),
        "confirmed_that_are_runs_of_one_symbol": sum(1 for p in patterns if p["held_out_confirmed"] and len(set(p["symbols"])) == 1),
        "held_out_confirmed": found["held_out"]["confirmed"],
        "coverage_searched": round(found["coverage"], 4), "coverage_held_out": round(found["held_out"]["coverage"], 4),
        "patterns": patterns,
    }


def main():
    source, out = sys.argv[1], sys.argv[2]
    rows, readings = read(source)
    report = {
        "data": "OpenFEMA Disaster Declarations Summaries v2 (real data; the API is live, so a later fetch has more rows)",
        "file_sha256": hashlib.sha256(open(source, "rb").read()).hexdigest(), "rows": rows,
        "left_out": "incidentType Biological",
        "split": str(SPLIT), "shuffle_seeds": SEEDS,
        "kute": kute.__version__,
        **{name: reading(readings[name], params) for name, params in READINGS.items()},
    }
    json.dump(report, open(out, "w", encoding="utf-8"), indent=1)
    for name in READINGS:
        r = report[name]
        print(f"{name}: {r['searched_events']} events searched in {r['searched_records']} records; {r['patterns_found']} patterns, "
              f"{r['patterns_with_each_record_shuffled_mean']:.1f} with each record shuffled, {r['patterns_with_disasters_shuffled_mean']:.1f} with the disasters shuffled; {r['held_out_confirmed']} confirmed on {r['held_out_events']} later events")


if __name__ == "__main__":
    main()
