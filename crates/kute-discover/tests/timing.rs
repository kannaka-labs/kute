//! Typed event streams: grouping, time order, and how long patterns take.

use kute_discover::{discover, timing, Params, PatternKind};
use kute_types::{EventSchema, EventStream, TimeUnit};

fn schema() -> EventSchema {
    EventSchema {
        id: "kute:test:machines".into(),
        version: 1,
        symbol_field: "event".into(),
        time_field: Some("t".into()),
        time_unit: Some(TimeUnit::Milliseconds),
        group_field: Some("machine".into()),
    }
}

/// Two machines, interleaved in the file. Each runs START, HEAT, DONE over
/// and over: HEAT comes 10 ms after START, DONE 200 ms after HEAT on
/// machine a and 300 ms on machine b.
fn interleaved() -> String {
    let mut lines = Vec::new();
    for cycle in 0..40i64 {
        for (machine, slow) in [("a", 200), ("b", 300)] {
            let t0 = cycle * 1000;
            lines.push(format!(r#"{{"machine":"{machine}","event":"START","t":{t0}}}"#));
            lines.push(format!(r#"{{"machine":"{machine}","event":"HEAT","t":{}}}"#, t0 + 10));
            lines.push(format!(r#"{{"machine":"{machine}","event":"DONE","t":{}}}"#, t0 + 10 + slow));
        }
    }
    lines.join("\n")
}

fn stream(text: &str) -> EventStream {
    let mut s = EventStream::new(schema()).unwrap();
    s.read_jsonl(text.as_bytes(), "test").unwrap();
    s.validate().unwrap();
    s
}

#[test]
fn grouping_separates_what_the_file_interleaves() {
    let s = stream(&interleaved());
    assert_eq!(s.records.len(), 2);
    assert_eq!(s.records[0].key.as_deref(), Some("a"));
    assert_eq!(s.events(), 240);
    // Read as one flat sequence the file says START START HEAT HEAT...;
    // grouped, each machine shows its own cycle.
    assert_eq!(&s.records[1].symbols[..4], ["START", "HEAT", "DONE", "START"]);
    assert_eq!(&s.records[1].times[..4], [0, 10, 310, 1000]);
}

#[test]
fn patterns_report_how_long_they_take() {
    let s = stream(&interleaved());
    let found = discover(&s.symbol_records(), &Params { min_support: 5, ..Params::default() }).unwrap();
    let timings = timing(&found, &s).unwrap();
    assert_eq!(timings.len(), found.patterns.len());
    let of = |symbols: &[&str], kind: PatternKind| {
        let i = found.patterns.iter().position(|p| p.kind == kind && p.symbols == symbols).unwrap_or_else(|| panic!("no pattern {symbols:?}"));
        timings[i].duration.clone().unwrap()
    };
    // START HEAT always continues to DONE, so discovery reports the whole
    // cycle and not its prefix. Two cycles in a row span one period more.
    let two = of(&["START", "HEAT", "DONE", "START", "HEAT", "DONE"], PatternKind::Contiguous);
    assert_eq!((two.occurrences, two.min, two.max), (78, 1210, 1310));
    // Half the cycles take 210 ms and half 310: the spread is in the report.
    let cycle = of(&["START", "HEAT", "DONE"], PatternKind::Contiguous);
    assert_eq!((cycle.occurrences, cycle.min, cycle.max), (80, 210, 310));
    assert_eq!((cycle.p10, cycle.p90), (210, 310));
}

#[test]
fn events_out_of_time_order_are_sorted_and_ties_keep_file_order() {
    let text = r#"{"machine":"a","event":"C","t":30}
{"machine":"a","event":"A","t":10}
{"machine":"a","event":"B1","t":20}
{"machine":"a","event":"B2","t":20}"#;
    let s = stream(text);
    assert_eq!(s.records[0].symbols, ["A", "B1", "B2", "C"]);
    assert_eq!(s.records[0].times, [10, 20, 20, 30]);
}

#[test]
fn the_schema_is_part_of_the_identity() {
    let text = interleaved();
    let a = stream(&text);
    let mut other = schema();
    other.group_field = None;
    let mut b = EventStream::new(other).unwrap();
    b.read_jsonl(text.as_bytes(), "test").unwrap();
    assert_eq!(b.records.len(), 1);
    assert_ne!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
    assert_eq!(a.fingerprint().unwrap(), stream(&text).fingerprint().unwrap());
    // A stream survives its own serialization.
    let json = serde_json::to_vec(&a).unwrap();
    assert_eq!(serde_json::from_slice::<EventStream>(&json).unwrap(), a);
}

#[test]
fn nothing_is_skipped_or_rounded() {
    let read = |text: &str| {
        let mut s = EventStream::new(schema()).unwrap();
        s.read_jsonl(text.as_bytes(), "test").map(|_| s)
    };
    assert!(read(r#"{"machine":"a","event":"A","t":1.5}"#).unwrap_err().to_string().contains("whole number"));
    assert!(read(r#"{"machine":"a","t":1}"#).unwrap_err().to_string().contains("event"));
    assert!(read(r#"{"machine":"a","event":"A"}"#).unwrap_err().to_string().contains("\"t\""));
    assert!(read(r#"{"event":"A","t":1}"#).unwrap_err().to_string().contains("machine"));
    assert!(read(r#"{"machine":"a","event":7,"t":1}"#).is_err());
    assert!(read("[1,2]").is_err());
    // Whole numbers written as floats are whole numbers; blank lines are not events.
    let s = read("{\"machine\":1,\"event\":\"A\",\"t\":2.0}\n\n").unwrap();
    assert_eq!((s.records[0].key.as_deref(), s.records[0].times[0]), (Some("1"), 2));
    // A time field without a unit is not a schema.
    let mut bad = schema();
    bad.time_unit = None;
    assert!(EventStream::new(bad).is_err());
}

#[test]
fn timing_needs_timestamps() {
    let mut untimed = schema();
    untimed.time_field = None;
    untimed.time_unit = None;
    let mut s = EventStream::new(untimed).unwrap();
    s.read_jsonl(interleaved().as_bytes(), "test").unwrap();
    assert!(s.records[0].times.is_empty());
    let found = discover(&s.symbol_records(), &Params::default()).unwrap();
    assert!(timing(&found, &s).is_err());
}

#[test]
fn a_duration_that_does_not_fit_is_an_error_not_a_panic() {
    let mut text = String::new();
    for machine in 0..60 {
        for _ in 0..3 {
            text.push_str(&format!("{{\"machine\":\"m{machine}\",\"event\":\"START\",\"t\":{}}}\n", i64::MIN));
            text.push_str(&format!("{{\"machine\":\"m{machine}\",\"event\":\"DONE\",\"t\":{}}}\n", i64::MAX));
        }
    }
    let s = stream(&text);
    let d = discover(&s.symbol_records(), &Params::default()).unwrap();
    assert!(!d.patterns.is_empty());
    assert!(timing(&d, &s).is_err());
}

#[test]
fn split_last_keeps_a_record_on_each_side() {
    assert!(EventStream::new(schema()).unwrap().split_last(0.5).is_err());
    assert!(stream(r#"{"machine":"a","event":"START","t":1}"#).split_last(0.5).is_err());
    let two = stream("{\"machine\":\"a\",\"event\":\"START\",\"t\":1}\n{\"machine\":\"b\",\"event\":\"START\",\"t\":2}");
    for fraction in [0.01, 0.5, 0.99] {
        let (head, tail) = two.clone().split_last(fraction).unwrap();
        assert_eq!((head.records.len(), tail.records.len()), (1, 1));
    }
}
