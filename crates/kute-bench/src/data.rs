//! Seeded synthetic datasets. Same seed, same bytes, on every machine.
//!
//! These exist so the benchmark program runs anywhere with no downloads. They
//! are built from small templates, so they are far more regular than real
//! data and flatter any tokenizer. Reports mark them `synthetic: true`; treat
//! their compression numbers as a check that the machinery works, not as a
//! prediction for real corpora.

/// xorshift64*: small, fast, good enough for test data. Not for anything else.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() >> 33) as usize) % n.max(1)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Uniform random bytes. The control case: nothing can compress these.
pub fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len).map(|_| (rng.next_u64() >> 40) as u8).collect()
}

/// Fixed-layout binary records, like a telemetry frame dump: magic, record
/// type, counter, a payload drawn from a small pool, a noisy field, padding.
pub fn binary_records(seed: u64, records: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let payloads: Vec<[u8; 8]> = (0..12).map(|_| rng.next_u64().to_le_bytes()).collect();
    let mut out = Vec::with_capacity(records * 32);
    for i in 0..records {
        out.extend_from_slice(b"KTF\x01");
        out.extend_from_slice(&[*rng.pick(&[0x10u8, 0x11, 0x20, 0x21, 0x7f]), 0x00]);
        out.extend_from_slice(&(i as u32).to_le_bytes());
        let payload: &[u8; 8] = rng.pick(&payloads);
        out.extend_from_slice(payload);
        out.extend_from_slice(&((rng.next_u64() >> 48) as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 12]);
    }
    out
}

/// A plant alarm log as JSONL. Events arrive in recurring episodes (start-up,
/// trip, valve fault) with unrelated events in between.
pub fn alarm_log(seed: u64, episodes: usize) -> Vec<u8> {
    const EPISODES: &[&[&str]] = &[
        &["START_CMD", "RUN_FB", "SPEED_OK"],
        &["CURRENT_HI", "MOTOR_TRIP", "OPERATOR_ACK", "RESET_CMD"],
        &["VALVE_OPEN_CMD", "VALVE_TRAVEL_TIMEOUT", "VALVE_FAULT", "OPERATOR_ACK"],
        &["TANK_LEVEL_HI", "PUMP_STOP_CMD", "RUN_FB_LOST"],
        &["COMM_LOSS", "COMM_RESTORED"],
    ];
    const NOISE: &[&str] = &["DOOR_OPEN", "HEARTBEAT", "TEMP_WARN", "FILTER_DP_HI", "SHIFT_CHANGE"];
    const UNITS: &[&str] = &["P-101", "P-102", "M-201", "V-310", "TK-400"];
    let mut rng = Rng::new(seed);
    let mut out = String::new();
    let mut t = 1_700_000_000u64;
    for _ in 0..episodes {
        let unit = rng.pick(UNITS);
        for tag in *rng.pick(EPISODES) {
            t += 1 + rng.below(5) as u64;
            out.push_str(&format!("{{\"ts\":{t},\"unit\":\"{unit}\",\"tag\":\"{tag}\"}}\n"));
        }
        for _ in 0..rng.below(3) {
            t += 1 + rng.below(30) as u64;
            out.push_str(&format!("{{\"ts\":{t},\"unit\":\"{}\",\"tag\":\"{}\"}}\n", rng.pick(UNITS), rng.pick(NOISE)));
        }
    }
    out.into_bytes()
}

/// Agent-to-agent messages as JSONL: a fixed envelope with varying fields.
pub fn agent_messages(seed: u64, messages: usize) -> Vec<u8> {
    const AGENTS: &[&str] = &["kannaka", "flaukowski", "queen-sync", "observer-7", "courier-2"];
    const INTENTS: &[&str] = &["request_observation", "report_observation", "propose_vocabulary", "accept", "decline", "translate"];
    const TOOLS: &[&str] = &["kute_encode", "kute_decode", "kute_inspect", "kute_compare", "sensor_read", "memory_recall"];
    const TARGETS: &[&str] = &["sensor-1", "sensor-2", "tank-400", "radio-desk", "ledger"];
    let mut rng = Rng::new(seed);
    let mut out = String::new();
    for i in 0..messages {
        let from = rng.pick(AGENTS);
        let to = rng.pick(AGENTS);
        out.push_str(&format!(
            "{{\"seq\": {i}, \"from\": \"{from}\", \"to\": \"{to}\", \"intent\": \"{}\", \"tool\": \"{}\", \"args\": {{\"target\": \"{}\", \"window_s\": {}, \"confidence\": 0.{}}}}}\n",
            rng.pick(INTENTS),
            rng.pick(TOOLS),
            rng.pick(TARGETS),
            10 * (1 + rng.below(12)),
            10 + rng.below(90),
        ));
    }
    out.into_bytes()
}

/// Text made of invented words, enough distinct ones to fill a 32K vocabulary.
/// Used only to time loading a large vocabulary.
pub fn invented_words(seed: u64, words: usize) -> Vec<u8> {
    const ONSETS: &[&str] = &["b", "br", "c", "ch", "d", "f", "g", "gr", "h", "k", "l", "m", "n", "p", "pl", "r", "s", "st", "t", "tr", "v", "w", "z"];
    const VOWELS: &[&str] = &["a", "e", "i", "o", "u", "ai", "ea", "ou", "io"];
    const CODAS: &[&str] = &["", "n", "r", "s", "t", "l", "m", "nd", "st", "ck"];
    let mut rng = Rng::new(seed);
    let lexicon: Vec<String> = (0..40_000)
        .map(|_| {
            let mut w = String::new();
            for _ in 0..2 + rng.below(3) {
                w.push_str(rng.pick::<&str>(ONSETS));
                w.push_str(rng.pick::<&str>(VOWELS));
                w.push_str(rng.pick::<&str>(CODAS));
            }
            w
        })
        .collect();
    let mut out = String::new();
    for i in 0..words {
        // Skewed choice so word frequencies are uneven, as in language.
        let a = rng.below(lexicon.len());
        let b = rng.below(lexicon.len());
        out.push_str(&lexicon[a.min(b)]);
        out.push(if i % 17 == 16 { '\n' } else { ' ' });
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_reproducible() {
        assert_eq!(random_bytes(1, 64), random_bytes(1, 64));
        assert_ne!(random_bytes(1, 64), random_bytes(2, 64));
        assert_eq!(binary_records(3, 10), binary_records(3, 10));
        assert_eq!(alarm_log(4, 10), alarm_log(4, 10));
        assert_eq!(agent_messages(5, 10), agent_messages(5, 10));
        assert_eq!(invented_words(6, 100), invented_words(6, 100));
        for line in String::from_utf8(agent_messages(5, 10)).unwrap().lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok(), "{line}");
        }
        for line in String::from_utf8(alarm_log(4, 10)).unwrap().lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok(), "{line}");
        }
    }
}
