//! Gate AC-708 of the v0.7 specification: "Streaming discovery prototype
//! demonstrates bounded memory." All data here is synthetic and seeded.

use kute_discover::live::{Live, LiveParams};
use kute_discover::{discover, Params, PatternKind};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const CYCLE: [&str; 4] = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];

/// While no bound is reached, the stream gives exactly what the offline
/// method gives for contiguous patterns: same sequences, support, cohesion.
#[test]
fn within_its_bounds_the_stream_matches_offline_discovery() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let noise = ["IDLE", "TICK", "DOOR", "FAN", "LAMP"];
    // 40 devices, each a record; events of all devices interleaved.
    let mut records: Vec<Vec<String>> = vec![Vec::new(); 40];
    let mut arrivals: Vec<(usize, String)> = Vec::new();
    let mut pending: Vec<Vec<&str>> = vec![Vec::new(); 40];
    for _ in 0..30_000 {
        let device = rng.below(40) as usize;
        if pending[device].is_empty() {
            pending[device] = if rng.below(4) == 0 { CYCLE.iter().rev().copied().collect() } else { vec![noise[rng.below(5) as usize]] };
        }
        let symbol = pending[device].pop().unwrap().to_string();
        records[device].push(symbol.clone());
        arrivals.push((device, symbol));
    }
    let mut live = Live::new(LiveParams { max_length: 6, ..LiveParams::default() }).unwrap();
    for (device, symbol) in &arrivals {
        live.observe(&format!("device-{device}"), symbol);
    }
    let report = live.report(5, 1.0, 10_000);
    assert!(report.exact, "{report:?}");
    assert_eq!(report.events, 30_000);
    assert!(report.max_undercount.iter().all(|&d| d == 0));

    let offline = discover(&records, &Params { min_support: 5, max_length: 6, min_cohesion_bits: 1.0, window: 0, max_patterns: 10_000, ..Params::default() }).unwrap();
    let mut expected: Vec<(Vec<String>, u64, f64)> = offline.patterns.iter().filter(|p| p.kind == PatternKind::Contiguous).map(|p| (p.symbols.clone(), p.support, p.cohesion_bits)).collect();
    let mut found: Vec<(Vec<String>, u64, f64)> = report.patterns.iter().map(|p| (p.symbols.clone(), p.support, p.cohesion_bits)).collect();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    found.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!expected.is_empty());
    assert_eq!(found.len(), expected.len());
    for (f, e) in found.iter().zip(&expected) {
        assert_eq!((&f.0, f.1), (&e.0, e.1));
        assert!((f.2 - e.2).abs() < 1e-9, "{f:?} {e:?}");
    }
    assert!(report.patterns.iter().any(|p| p.symbols == CYCLE), "the planted cycle is found");
    // Ranked the same way, too.
    assert_eq!(report.patterns[0].symbols, offline.patterns[0].symbols);
    assert_eq!(report.patterns[0].content_hash, offline.patterns[0].content_hash);
}

/// The demonstration: a long stream with far more symbols, sequences and
/// entities than the bounds admit. The state never passes its bound, the
/// report says what was approximated, the bounds on support hold, and the
/// planted pattern is still found.
#[test]
fn state_stays_within_its_bound_on_a_stream_that_exceeds_every_limit() {
    let params = LiveParams { max_length: 5, max_symbols: 2_000, max_symbol_bytes: 32, max_candidates: 4_000, max_entities: 500 };
    let bound = params.state_bytes_bound();
    let mut live = Live::new(params.clone()).unwrap();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    // 20,000 entities, 300,000 rare symbols and 1,500 common ones whose
    // pairs alone number millions: none of it fits.
    let mut pending: std::collections::HashMap<u64, Vec<&str>> = std::collections::HashMap::new();
    let (mut planted, mut peak, events) = (0u64, 0u64, 2_000_000u64);
    for event in 0..events {
        // A few busy devices run the cycle; the crowd makes noise.
        let (entity, symbol): (u64, String) = if rng.below(5) == 0 {
            let device = rng.below(20);
            let queue = pending.entry(device).or_default();
            if queue.is_empty() {
                *queue = CYCLE.iter().rev().copied().collect();
            }
            let symbol = queue.pop().unwrap();
            if queue.is_empty() {
                planted += 1;
            }
            (device, symbol.to_string())
        } else if rng.below(8) == 0 {
            // One talkative entity strings common symbols together: more
            // distinct sequences than any table holds.
            (30, format!("C{}", rng.below(1_500)))
        } else {
            let symbol = if rng.below(2) == 0 { format!("S{}", rng.below(300_000)) } else { format!("C{}", rng.below(1_500)) };
            (50 + rng.below(20_000), symbol)
        };
        live.observe(&format!("e{entity}"), &symbol);
        if event % 50_000 == 0 || event == events - 1 {
            let report = live.report(1_000_000, 0.0, 0);
            assert!(report.state_bytes <= bound, "state {} passed the bound {bound} at event {event}", report.state_bytes);
            assert!(report.alphabet <= 2_000 && report.entities_tracked <= 500);
            assert!(report.sequences_held.iter().all(|&held| held <= 4_000));
            peak = peak.max(report.state_bytes);
        }
    }
    let report = live.report(1_000, 1.0, 20);
    assert!(!report.exact);
    assert!(report.events_folded > 500_000, "most rare symbols cannot be named: {}", report.events_folded);
    assert!(report.entities_evicted > 10_000);
    assert!(report.max_undercount.iter().any(|&d| d > 0));
    assert_eq!(report.state_bytes_bound, bound);
    // The busy devices are evicted now and then by the crowd, so some
    // cycles are cut; the count is a lower bound and the stated upper
    // bound still has to hold for what was offered.
    let cycle = report.patterns.iter().find(|p| p.symbols == CYCLE).unwrap_or_else(|| panic!("the planted cycle was not found: {:?}", report.patterns));
    assert!(cycle.support <= planted, "support {} is above the true {planted}", cycle.support);
    assert!(cycle.support * 10 >= planted * 9, "support {} lost more than a tenth of {planted}", cycle.support);
    // The stated upper bound covers the counting error only; cycles cut
    // by an eviction are outside it, which is why it can be below the truth.
    assert!(cycle.support_at_most >= cycle.support);
    println!(
        "{events} events, {planted} planted cycles, counted {} (at most {}); folded {}, evicted {}; undercount by length {:?}; peak state {peak} of {bound} bytes",
        cycle.support, cycle.support_at_most, report.events_folded, report.entities_evicted, report.max_undercount
    );
}

/// A count is never above the truth and never below it by more than the
/// reported undercount, checked against exact counts on a stream whose
/// sequences do not fit the table.
#[test]
fn reported_bounds_on_support_contain_the_true_count() {
    let mut rng = Rng(0x243f_6a88_85a3_08d3);
    let symbols: Vec<String> = (0..60).map(|i| format!("S{i}")).collect();
    // Skewed: low numbers are common, so some pairs are frequent.
    let stream: Vec<String> = (0..200_000).map(|_| symbols[(rng.below(60) * rng.below(60) / 60) as usize].clone()).collect();
    let mut live = Live::new(LiveParams { max_length: 3, max_candidates: 300, ..LiveParams::default() }).unwrap();
    for s in &stream {
        live.observe("one", s);
    }
    let report = live.report(1, f64::NEG_INFINITY, usize::MAX);
    assert!(!report.exact && report.max_undercount[1] > 0 && report.max_undercount[0] == 0, "{:?}", report.max_undercount);
    let mut truth: std::collections::HashMap<Vec<String>, u64> = std::collections::HashMap::new();
    for n in 2..=3 {
        for w in stream.windows(n) {
            *truth.entry(w.to_vec()).or_default() += 1;
        }
    }
    assert!(report.patterns.len() > 100);
    for p in &report.patterns {
        let t = truth[&p.symbols];
        assert!(p.support <= t && t <= p.support_at_most, "{:?}: {} <= {t} <= {}", p.symbols, p.support, p.support_at_most);
    }
    // Anything that truly occurs more often than the undercount is held.
    let held: std::collections::HashSet<&Vec<String>> = report.patterns.iter().map(|p| &p.symbols).collect();
    let live_counts = live.report(1, f64::NEG_INFINITY, usize::MAX);
    for (seq, &t) in &truth {
        if t > live_counts.max_undercount[seq.len() - 1] {
            assert!(held.contains(seq), "{seq:?} occurs {t} times and is not held");
        }
    }
}

/// A restart in the middle changes nothing, and a damaged checkpoint is
/// refused, not loaded.
#[test]
fn a_checkpoint_restores_the_same_state() {
    let params = LiveParams { max_length: 4, max_symbols: 50, max_candidates: 200, max_entities: 20, max_symbol_bytes: 16 };
    let mut rng = Rng(0x1319_8a2e_0370_7344);
    let events: Vec<(String, String)> = (0..40_000).map(|_| (format!("e{}", rng.below(60)), format!("S{}", rng.below(80)))).collect();
    let mut straight = Live::new(params.clone()).unwrap();
    let mut restarted = Live::new(params).unwrap();
    for (i, (entity, symbol)) in events.iter().enumerate() {
        straight.observe(entity, symbol);
        restarted.observe(entity, symbol);
        if i == 17_000 {
            let saved = restarted.checkpoint().unwrap();
            restarted = Live::restore(&saved).unwrap();
            assert_eq!(restarted.checkpoint().unwrap(), saved, "a checkpoint is a fixed point");
        }
    }
    assert_eq!(straight.checkpoint().unwrap(), restarted.checkpoint().unwrap());
    assert_eq!(straight.report(2, 0.0, 100), restarted.report(2, 0.0, 100));

    let saved = straight.checkpoint().unwrap();
    assert!(Live::restore(&saved[..saved.len() / 2]).is_err());
    let text = String::from_utf8(saved).unwrap();
    for (from, to) in [("\"max_candidates\":200", "\"max_candidates\":1"), ("\"max_symbols\":50", "\"max_symbols\":3"), ("\"format\":1", "\"format\":9"), ("\"max_length\":4", "\"max_length\":3")] {
        assert!(text.contains(from), "{from}");
        assert!(Live::restore(text.replacen(from, to, 1).as_bytes()).is_err(), "{to} was accepted");
    }
}
