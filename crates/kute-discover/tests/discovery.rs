//! Discovery must find structure that was put there, with correct evidence,
//! must not find it in shuffled data, and must hold up on held-out data.

use kute_discover::{discover, evaluate, observe, shuffled, split_holdout, Params, PatternKind};

/// Deterministic generator (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const MOTIF: [&str; 4] = ["PUMP_START", "PRESSURE_RISE", "VALVE_OPEN", "FLOW_OK"];

/// Synthetic: forty noise symbols drawn uniformly, with a four-symbol motif
/// inserted at random, and a gapped dependency (DOOR_OPEN, then DOOR_ALARM
/// three to five symbols later).
fn planted(seed: u64, length: usize) -> Vec<String> {
    let mut rng = Rng(seed);
    let mut out: Vec<String> = Vec::with_capacity(length + 8);
    let mut alarm_at: Option<usize> = None;
    while out.len() < length {
        if alarm_at == Some(out.len()) {
            out.push("DOOR_ALARM".into());
            alarm_at = None;
            continue;
        }
        match rng.below(100) {
            0..=3 if alarm_at.is_none() => out.extend(MOTIF.iter().map(|s| s.to_string())),
            4..=6 if alarm_at.is_none() => {
                out.push("DOOR_OPEN".into());
                alarm_at = Some(out.len() + 2 + rng.below(3) as usize);
            }
            _ => out.push(format!("N{:02}", rng.below(40))),
        }
    }
    out
}

fn text(symbols: &[String]) -> String {
    symbols.join(" ")
}

#[test]
fn finds_the_planted_motif_with_correct_evidence() {
    let stream = planted(7, 20_000);
    let records = vec![stream.clone()];
    let found = discover(&records, &Params::default()).unwrap();

    let top = &found.patterns[0];
    assert_eq!(top.kind, PatternKind::Contiguous);
    assert_eq!(top.symbols, MOTIF, "the strongest pattern should be the whole motif, got {}", text(&top.symbols));
    // Support is the true number of occurrences.
    let actual = stream.windows(4).filter(|w| w.iter().map(String::as_str).eq(MOTIF)).count() as u64;
    assert_eq!(top.support, actual);
    assert!(top.cohesion_bits > 4.0);
    // Evidence points at real occurrences.
    assert!(!top.examples.is_empty());
    for e in &top.examples {
        assert_eq!(e.record, 0);
        assert_eq!(&stream[e.offset as usize..e.offset as usize + 4], MOTIF);
    }
    // Parts of the motif occur exactly as often as the motif and are absorbed.
    assert!(!found.patterns.iter().any(|p| p.kind == PatternKind::Contiguous && p.symbols.len() < 4 && p.symbols.iter().all(|s| MOTIF.contains(&s.as_str()))));
    // The gapped dependency is found as a gapped pair.
    let gapped = found.patterns.iter().find(|p| p.kind == PatternKind::Gapped).expect("a gapped pattern");
    assert_eq!(gapped.symbols, ["DOOR_OPEN", "DOOR_ALARM"]);
    // The same content hash a vocabulary token with this payload would have.
    assert!(top.content_hash.starts_with("sha256:"));
    assert_eq!(found.candidates()[0], MOTIF);
}

#[test]
fn shuffled_control_reports_almost_nothing() {
    let records = vec![planted(7, 20_000)];
    let real = discover(&records, &Params::default()).unwrap();
    let control = discover(&shuffled(&records, 1), &Params::default()).unwrap();
    assert_eq!(control.observation.symbols, real.observation.symbols);
    assert_eq!(control.observation.alphabet, real.observation.alphabet);
    let strongest_real = real.patterns[0].evidence_bits;
    let strongest_control = control.patterns.iter().map(|p| p.evidence_bits).fold(0.0, f64::max);
    assert!(strongest_control < strongest_real / 20.0, "control {strongest_control} vs real {strongest_real}");
    assert!(control.coverage < real.coverage / 4.0);
    // The shuffle is seeded.
    assert_eq!(shuffled(&records, 1), shuffled(&records, 1));
    assert_ne!(shuffled(&records, 1), shuffled(&records, 2));
}

#[test]
fn patterns_hold_on_data_they_were_not_found_in() {
    let records = vec![planted(11, 40_000)];
    let (head, tail) = split_holdout(&records, 0.25).unwrap();
    assert_eq!(head[0].len() + tail[0].len(), records[0].len());
    let found = discover(&head, &Params::default()).unwrap();
    let report = evaluate(&found, &tail).unwrap();
    assert_eq!(report.patterns.len(), found.patterns.len());
    let motif = &report.patterns[0];
    assert!(motif.confirmed && motif.support > 50);
    let evidence = motif.evidence_bits.expect("a pattern that occurs has evidence");
    assert!((evidence - motif.support as f64 * motif.cohesion_bits.unwrap()).abs() < 1e-9);
    assert!(report.patterns.iter().filter(|p| p.support == 0).all(|p| p.evidence_bits.is_none()));
    let direct = tail[0].windows(4).filter(|w| w.iter().map(String::as_str).eq(MOTIF)).count() as u64;
    assert_eq!(motif.support, direct);
    assert!(report.coverage > 0.05);

    // Patterns from the shuffled control do not hold up.
    let control = discover(&shuffled(&head, 3), &Params::default()).unwrap();
    let control_report = evaluate(&control, &tail).unwrap();
    assert!(control_report.confirmed <= 2 || control_report.confirmed_fraction < report.confirmed_fraction);
    assert!(control_report.coverage < report.coverage / 2.0);
}

#[test]
fn observation_measures_order() {
    let ordered: Vec<String> = (0..3000).map(|i| ["A", "B", "C"][i % 3].to_string()).collect();
    let o = observe(std::slice::from_ref(&ordered));
    assert_eq!((o.records, o.symbols, o.alphabet), (1, 3000, 3));
    assert!((o.entropy_bits - 3f64.log2()).abs() < 1e-9);
    assert!(o.conditional_entropy_bits < 1e-9, "a strict cycle is fully predictable");
    assert!(o.fingerprint.starts_with("sha256:"));
    let mixed = observe(&shuffled(std::slice::from_ref(&ordered), 5));
    assert!(mixed.conditional_entropy_bits > 1.5);
}

#[test]
fn limits_and_bad_input() {
    let records = vec![planted(3, 5_000)];
    for bad in [
        Params { min_support: 1, ..Params::default() },
        Params { max_length: 1, ..Params::default() },
        Params { max_length: 65, ..Params::default() },
        Params { window: 1, ..Params::default() },
        Params { min_cohesion_bits: f64::NAN, ..Params::default() },
    ] {
        assert!(discover(&records, &bad).is_err());
    }
    let tight = Params { max_candidates: 10, ..Params::default() };
    assert!(discover(&records, &tight).is_err(), "the candidate limit must stop the run");
    assert!(discover(&[vec![String::new()]], &Params::default()).is_err());
    // Empty and tiny corpora are not errors.
    assert!(discover(&[], &Params::default()).unwrap().patterns.is_empty());
    assert!(discover(&[vec!["A".into()]], &Params::default()).unwrap().patterns.is_empty());
    assert!(split_holdout(&records, 1.0).is_err());
    // Multiple records: patterns never span a record boundary.
    let many: Vec<Vec<String>> = (0..50).map(|_| vec!["X".to_string(), "Y".to_string()]).collect();
    let found = discover(&many, &Params { window: 0, ..Params::default() }).unwrap();
    assert!(found.patterns.iter().all(|p| p.symbols == ["X", "Y"]));
    assert_eq!(found.patterns[0].records, 50);
}
