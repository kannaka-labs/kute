//! Prediction on held-out time (v0.7 specification, D2). Synthetic data.

use kute_discover::predict::{evaluate, PredictParams};
use kute_types::{EventSchema, EventStream, TimeUnit};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

fn stream(lines: &str) -> EventStream {
    let schema = EventSchema { id: "test:predict".into(), version: 1, symbol_field: "e".into(), time_field: Some("t".into()), time_unit: Some(TimeUnit::parse("s").unwrap()), group_field: Some("id".into()) };
    let mut stream = EventStream::new(schema).unwrap();
    stream.read_jsonl(lines.as_bytes(), "test").unwrap();
    stream
}

const NAMES: [&str; 4] = ["A", "B", "C", "D"];

/// Machines whose next event depends on their last two: after (x, y)
/// comes NAMES[(x + 2y) % 4] nine times in ten, anything otherwise.
/// With `structure` false every event is drawn at random.
fn events(seed: u64, machines: u64, count: u64, structure: bool) -> String {
    let mut rng = Rng(seed);
    let mut last: Vec<(u64, u64)> = (0..machines).map(|m| (m % 4, (m / 4) % 4)).collect();
    let mut out = String::new();
    for t in 0..count {
        // Later machines start later, so some are first seen after the cut.
        let alive = (machines * (t + count / 4) / count).clamp(1, machines);
        let m = rng.below(alive) as usize;
        let (x, y) = last[m];
        let next = if structure && rng.below(10) != 0 { (x + 2 * y) % 4 } else { rng.below(4) };
        last[m] = (y, next);
        out.push_str(&format!("{{\"id\":\"m{m}\",\"e\":\"{}\",\"t\":{t}}}\n", NAMES[next as usize]));
    }
    out
}

#[test]
fn structure_two_events_deep_is_found_by_the_context_and_not_by_the_baselines() {
    let report = evaluate(&stream(&events(7, 40, 60_000, true)), &PredictParams::default()).unwrap();
    assert_eq!(report.held_out_events + report.counted_events, 60_000);
    assert!((report.held_out_events as f64 / 60_000.0 - 0.25).abs() < 0.01);
    assert_eq!(report.alphabet, 4);
    // Four events about equally common: two bits without the past. One
    // event back says little; two say most of it.
    assert!((report.frequency.bits_per_event - 2.0).abs() < 0.05, "{:?}", report.frequency);
    assert!(report.previous.bits_per_event > 1.8, "{:?}", report.previous);
    assert!(report.context.bits_per_event < 0.9, "{:?}", report.context);
    assert!(report.context.top1_accuracy > 0.88 && report.context.top1_accuracy < 0.96);
    assert!(report.frequency.top1_accuracy < 0.35);
    // It says "about 0.9" and is right about 0.9 of the time.
    assert!(report.context.calibration_error < 0.03, "{:?}", report.context_calibration);
    let d = &report.context_over_previous;
    assert!(d.bits_per_event > 0.9 && d.interval_95.0 > 0.8 && d.interval_95.0 <= d.bits_per_event && d.bits_per_event <= d.interval_95.1, "{d:?}");
    // Machines first seen after the cut are predicted as well as the rest:
    // the structure belongs to the process, not to the machine.
    assert!(report.unseen_entities > 0 && report.unseen_entities < report.held_out_entities);
    let unseen = report.unseen_entities_only.as_ref().unwrap();
    assert!(unseen[2].events > 100 && unseen[2].bits_per_event < 1.0, "{unseen:?}");
    // No drift was planted, and none shows.
    assert_eq!(report.windows.len(), 4);
    assert!(report.windows.iter().all(|w| (w.context_bits - report.context.bits_per_event).abs() < 0.15), "{:?}", report.windows);
    assert!(report.windows.windows(2).all(|w| w[0].to_time <= w[1].from_time));
    assert_eq!(report.windows.iter().map(|w| w.events).sum::<u64>(), report.held_out_events);
    // The same data and settings give the same report.
    assert_eq!(evaluate(&stream(&events(7, 40, 60_000, true)), &PredictParams::default()).unwrap(), report);
}

#[test]
fn where_there_is_nothing_to_find_the_context_gains_nothing() {
    let report = evaluate(&stream(&events(11, 40, 60_000, false)), &PredictParams::default()).unwrap();
    assert!((report.frequency.bits_per_event - 2.0).abs() < 0.01);
    // Looking further back cannot help and costs a little.
    let d = &report.context_over_previous;
    assert!(d.bits_per_event < 0.001 && d.bits_per_event > -0.02, "{d:?}");
    assert!(d.interval_95.1 < 0.005, "{d:?}");
    assert!(report.context.top1_accuracy < 0.3);
    assert!(report.context.bits_per_event >= 1.99);
}

#[test]
fn a_predictor_that_goes_stale_shows_it_in_the_windows_and_new_names_are_counted() {
    // The rule holds before the cut and for a while after, then reverses;
    // late in the stream a name appears that was never seen.
    let mut rng = Rng(3);
    let mut out = String::new();
    let mut last = [0u64; 20];
    for t in 0..40_000u64 {
        let m = rng.below(20) as usize;
        let rule = if t < 35_000 { (last[m] + 1) % 4 } else { (last[m] + 3) % 4 };
        let next = if rng.below(10) != 0 { rule } else { rng.below(4) };
        last[m] = next;
        let name = if t > 39_000 && rng.below(20) == 0 { "NEW" } else { NAMES[next as usize] };
        out.push_str(&format!("{{\"id\":\"m{m}\",\"e\":\"{name}\",\"t\":{t}}}\n"));
    }
    let report = evaluate(&stream(&out), &PredictParams { windows: 4, ..PredictParams::default() }).unwrap();
    let w = &report.windows;
    assert!(w[0].context_bits < 1.0 && w[1].context_bits < 1.0, "{w:?}");
    assert!(w[2].context_bits > 3.0 && w[3].context_bits > 3.0, "{w:?}");
    // Stale, the predictor is worse than knowing nothing.
    assert!(w[3].context_bits > w[3].frequency_bits);
    assert!(report.unseen_symbol_events > 20);
    assert_eq!(report.alphabet, 4);
    assert!(report.context.bits_per_event.is_finite());
}

#[test]
fn what_cannot_be_measured_is_refused() {
    let lines = events(1, 5, 500, true);
    let good = stream(&lines);
    for bad in [
        PredictParams { holdout_fraction: 0.0, ..PredictParams::default() },
        PredictParams { holdout_fraction: 1.0, ..PredictParams::default() },
        PredictParams { max_context: 0, ..PredictParams::default() },
        PredictParams { max_context: 9, ..PredictParams::default() },
        PredictParams { min_count: 0, ..PredictParams::default() },
        PredictParams { windows: 0, ..PredictParams::default() },
    ] {
        assert!(evaluate(&good, &bad).is_err(), "{bad:?}");
    }
    // Without times there is no later to hold out.
    let mut untimed = EventStream::new(EventSchema { id: "x".into(), version: 1, symbol_field: "e".into(), time_field: None, time_unit: None, group_field: None }).unwrap();
    untimed.read_jsonl(lines.as_bytes(), "test").unwrap();
    assert!(evaluate(&untimed, &PredictParams::default()).is_err());
    // All events at one moment cannot be cut.
    let same: String = (0..50).map(|i| format!("{{\"id\":\"m{}\",\"e\":\"A\",\"t\":5}}\n", i % 3)).collect();
    assert!(evaluate(&stream(&same), &PredictParams::default()).is_err());
    assert!(evaluate(&stream("{\"id\":\"m\",\"e\":\"A\",\"t\":5}\n"), &PredictParams::default()).is_err());
}

/// A name is unknown only if it never occurred before the cut, in any
/// entity. Whether an entity comes early or late in the input does not
/// change what was known at the cut.
#[test]
fn a_name_seen_before_the_cut_by_a_later_entity_is_known() {
    // Entity `a` is listed first and uses B only after the cut; entity
    // `b`, listed second, used B before it.
    let mut lines = String::from("{\"id\":\"a\",\"e\":\"A\",\"t\":0}\n");
    for t in 1..=30 {
        lines.push_str(&format!("{{\"id\":\"b\",\"e\":\"{}\",\"t\":{t}}}\n", if t % 2 == 0 { "A" } else { "B" }));
    }
    for t in 31..=40 {
        lines.push_str(&format!("{{\"id\":\"a\",\"e\":\"B\",\"t\":{t}}}\n"));
    }
    let report = evaluate(&stream(&lines), &PredictParams { holdout_fraction: 0.25, resamples: 10, ..PredictParams::default() }).unwrap();
    assert_eq!(report.alphabet, 2);
    assert!(report.held_out_events >= 10);
    assert_eq!(report.unseen_symbol_events, 0, "B occurred fifteen times before the cut");
}
