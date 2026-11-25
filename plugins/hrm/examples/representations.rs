//! Measurements of the composition methods in `kute_hrm::compose`
//! (specification F1), written as JSON on standard output.
//!
//!     cargo run --release -p kute-hrm --example representations -- \
//!         LABEL events.jsonl GROUP_FIELD [LABEL events.jsonl GROUP_FIELD ...]
//!
//! Each events file is JSON Lines with an `event` field and the named
//! group field, in time order. With no files, only the synthetic parts run.
//!
//! Parts 1 to 3 use synthetic sequences of made-up tokens: they measure
//! the arithmetic, and say nothing about any data. Parts 4 and 5 use
//! windows of real event logs, against two exact baselines that use no
//! vectors at all:
//!
//!   counts      how many of each event a window has (order ignored)
//!   positions   which event is at each place (the thing a rotated sum
//!               approximates in fewer dimensions)
//!
//! Nothing here is learned, and no medium is simulated: the "memory" is
//! a plain sum of vectors and retrieval is an inner product.

use kute_hrm::compose::{normalize, similarity, Algorithm, Composer, Item};
use kute_types::TokenPayload;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

const DIMENSIONS: [usize; 3] = [256, 1024, 4096];
const WINDOW: usize = 6;
const TRIALS: usize = 200;
const QUERIES: usize = 500;
const ALPHABET: usize = 64;

/// The last query composed, kept so that it is composed once and not
/// once for each stored window.
type Composed = Option<(Vec<usize>, Vec<(f32, f32)>)>;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gaussian(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

fn token(composer: &Composer, name: &str) -> Item {
    composer.item(&TokenPayload::Symbols(vec![name.to_string()]).content_hash())
}

fn compose(composer: &Composer, alphabet: &[Item], sequence: &[usize]) -> Vec<(f32, f32)> {
    let parts: Vec<&[(f32, f32)]> = sequence.iter().map(|&i| alphabet[i].as_slice()).collect();
    composer.compose(&parts).expect("a sequence within bounds")
}

fn round(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Share of (stored, not stored) pairs the scores put in the right order; ties count half.
fn auc(stored: &[f64], others: &[f64]) -> f64 {
    let mut all: Vec<(f64, bool)> = stored.iter().map(|&s| (s, true)).chain(others.iter().map(|&s| (s, false))).collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut rank_sum, mut i) = (0.0f64, 0usize);
    while i < all.len() {
        let mut j = i;
        while j < all.len() && all[j].0 == all[i].0 {
            j += 1;
        }
        let rank = (i + j + 1) as f64 / 2.0;
        rank_sum += rank * all[i..j].iter().filter(|x| x.1).count() as f64;
        i = j;
    }
    let (n, m) = (stored.len() as f64, others.len() as f64);
    (rank_sum - n * (n + 1.0) / 2.0) / (n * m)
}

fn synthetic(rng: &mut Rng) -> Value {
    let names: Vec<String> = (0..ALPHABET).map(|i| format!("synthetic-token-{i}")).collect();
    let (mut order, mut recovery, mut noise, mut cost) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for dimension in DIMENSIONS {
        for algorithm in Algorithm::ALL {
            let composer = Composer::new(algorithm, dimension).unwrap();
            let alphabet: Vec<Item> = names.iter().map(|n| token(&composer, n)).collect();
            // 1. The same tokens in another order.
            let mut total = 0.0;
            for _ in 0..TRIALS {
                let mut sequence: Vec<usize> = Vec::new();
                while sequence.len() < WINDOW {
                    let t = rng.below(ALPHABET);
                    if !sequence.contains(&t) {
                        sequence.push(t);
                    }
                }
                let mut other = sequence.clone();
                other.rotate_left(1 + rng.below(WINDOW - 1));
                total += f64::from(similarity(&compose(&composer, &alphabet, &sequence), &compose(&composer, &alphabet, &other)).unwrap());
            }
            order.push(json!({ "algorithm": algorithm.name(), "dimension": dimension, "mean_similarity_to_the_same_tokens_reordered": round(total / TRIALS as f64) }));

            // 2. Finding a constituent again, by sequence length.
            for length in [2usize, 4, 8, 16, 32, 64] {
                let (mut right, mut asked) = (0usize, 0usize);
                for _ in 0..TRIALS / 4 {
                    let sequence: Vec<usize> = (0..length).map(|_| rng.below(ALPHABET)).collect();
                    let composed = compose(&composer, &alphabet, &sequence);
                    for position in 0..length.min(8) {
                        let position = position * length / length.min(8);
                        let known: Vec<(usize, &[(f32, f32)])> = sequence.iter().enumerate().filter(|(p, _)| *p != position).map(|(p, &i)| (p, alphabet[i].as_slice())).collect();
                        let found = composer.recover(&composed, position, &known, &alphabet).unwrap();
                        asked += 1;
                        right += usize::from(match algorithm {
                            Algorithm::Bundle => sequence.contains(&found),
                            _ => found == sequence[position],
                        });
                    }
                }
                recovery.push(json!({ "algorithm": algorithm.name(), "dimension": dimension, "sequence_length": length, "found": round(right as f64 / asked as f64) }));
            }

            // 3. The same, with noise added to the composed vector.
            if dimension == 1024 && algorithm != Algorithm::Bundle {
                for snr_db in [10i32, 0, -10, -20] {
                    let (mut right, mut asked) = (0usize, 0usize);
                    let sigma = (10f64.powf(-f64::from(snr_db) / 10.0) / (2.0 * dimension as f64)).sqrt();
                    for _ in 0..TRIALS / 2 {
                        let sequence: Vec<usize> = (0..8).map(|_| rng.below(ALPHABET)).collect();
                        let mut composed = compose(&composer, &alphabet, &sequence);
                        for v in composed.iter_mut() {
                            *v = (v.0 + (rng.gaussian() * sigma) as f32, v.1 + (rng.gaussian() * sigma) as f32);
                        }
                        let position = rng.below(8);
                        let known: Vec<(usize, &[(f32, f32)])> = sequence.iter().enumerate().filter(|(p, _)| *p != position).map(|(p, &i)| (p, alphabet[i].as_slice())).collect();
                        asked += 1;
                        right += usize::from(composer.recover(&composed, position, &known, &alphabet).unwrap() == sequence[position]);
                    }
                    noise.push(json!({ "algorithm": algorithm.name(), "dimension": dimension, "sequence_length": 8, "signal_to_noise_db": snr_db, "found": round(right as f64 / asked as f64) }));
                }
            }

            // 6. Cost, roughly: one thread, this machine.
            let sequence: Vec<usize> = (0..WINDOW).collect();
            let started = Instant::now();
            let mut last = Vec::new();
            for _ in 0..200 {
                last = compose(&composer, &alphabet, &sequence);
            }
            let composing = started.elapsed().as_nanos() as f64 / 200.0;
            let started = Instant::now();
            let mut sink = 0.0f32;
            for item in alphabet.iter().cycle().take(2000) {
                sink += similarity(&last, item).unwrap();
            }
            let comparing = started.elapsed().as_nanos() as f64 / 2000.0;
            cost.push(json!({ "algorithm": algorithm.name(), "dimension": dimension, "microseconds_to_compose_six_tokens": round(composing / 1000.0), "microseconds_per_similarity": round(comparing / 1000.0), "bytes_per_vector": dimension * 8, "_": sink.is_finite() }));
        }
    }
    json!({
        "data": "synthetic: made-up tokens drawn uniformly; measures the arithmetic only",
        "alphabet": ALPHABET,
        "chance_of_finding_a_token_by_luck": round(1.0 / ALPHABET as f64),
        "order_sensitivity": order,
        "constituent_recovery": recovery,
        "constituent_recovery_note": "bundle: the token found is somewhere in the sequence (it cannot say where). permute-bundle: the token at the asked position. bind-chain: the token at the asked position, given every other constituent.",
        "noise_tolerance": noise,
        "cost": cost,
    })
}

/// Distinct windows of `WINDOW` consecutive events of one entity.
fn windows_of(path: &str, group_field: &str) -> (Vec<String>, Vec<Vec<usize>>, u64) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut recent: HashMap<String, Vec<usize>> = HashMap::new();
    let mut seen: BTreeSet<Vec<usize>> = BTreeSet::new();
    let mut events = 0u64;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let record: Value = serde_json::from_str(line).expect("a JSON line");
        let name = record["event"].as_str().expect("an event field").to_string();
        let group = record[group_field].to_string();
        let next = ids.len();
        let id = *ids.entry(name).or_insert(next);
        let tail = recent.entry(group).or_default();
        tail.push(id);
        if tail.len() > WINDOW {
            tail.remove(0);
        }
        if tail.len() == WINDOW {
            seen.insert(tail.clone());
        }
        events += 1;
    }
    let mut names = vec![String::new(); ids.len()];
    for (name, id) in ids {
        names[id] = name;
    }
    (names, seen.into_iter().collect(), events)
}

fn real(label: &str, path: &str, group_field: &str, rng: &mut Rng) -> Value {
    let (names, mut windows, events) = windows_of(path, group_field);
    rng.shuffle(&mut windows);
    let symbols = names.len();
    // How many of the distinct windows are another's events in another order.
    let mut bags: BTreeMap<Vec<usize>, usize> = BTreeMap::new();
    for w in &windows {
        let mut bag = w.clone();
        bag.sort_unstable();
        *bags.entry(bag).or_default() += 1;
    }
    let sharing = windows.iter().filter(|w| { let mut bag = (*w).clone(); bag.sort_unstable(); bags[&bag] > 1 }).count();

    let histogram = |w: &[usize]| {
        let mut h = vec![0.0f64; symbols];
        for &s in w {
            h[s] += 1.0;
        }
        h
    };
    let (mut recognition, mut retrieval) = (Vec::new(), Vec::new());
    for stored_count in [10usize, 100, 1000, 10_000] {
        if windows.len() < 2 * stored_count {
            continue;
        }
        let (stored, others) = (&windows[..stored_count], &windows[stored_count..2 * stored_count]);
        // 4. One memory holding all the stored windows: is a window in it?
        let mut row = json!({ "windows_stored": stored_count, "windows_not_stored": stored_count });
        let mut counts_memory = vec![0.0f64; symbols];
        let mut position_memory = vec![vec![0.0f64; symbols]; WINDOW];
        for w in stored {
            for (p, &s) in w.iter().enumerate() {
                counts_memory[s] += 1.0;
                position_memory[p][s] += 1.0;
            }
        }
        let by_counts = |w: &Vec<usize>| {
            let h = histogram(w);
            h.iter().zip(&counts_memory).map(|(a, b)| a * b).sum::<f64>() / h.iter().map(|a| a * a).sum::<f64>().sqrt()
        };
        let by_positions = |w: &Vec<usize>| w.iter().enumerate().map(|(p, &s)| position_memory[p][s]).sum::<f64>();
        row["exact_counts"] = json!(round(auc(&stored.iter().map(by_counts).collect::<Vec<_>>(), &others.iter().map(by_counts).collect::<Vec<_>>())));
        row["exact_positions"] = json!(round(auc(&stored.iter().map(by_positions).collect::<Vec<_>>(), &others.iter().map(by_positions).collect::<Vec<_>>())));
        for dimension in DIMENSIONS {
            for algorithm in Algorithm::ALL {
                let composer = Composer::new(algorithm, dimension).unwrap();
                let alphabet: Vec<Item> = names.iter().map(|n| token(&composer, n)).collect();
                let mut memory = vec![(0.0f32, 0.0f32); dimension];
                for w in stored {
                    for (slot, v) in memory.iter_mut().zip(compose(&composer, &alphabet, w)) {
                        *slot = (slot.0 + v.0, slot.1 + v.1);
                    }
                }
                normalize(&mut memory).unwrap();
                let score = |w: &Vec<usize>| f64::from(similarity(&memory, &compose(&composer, &alphabet, w)).unwrap());
                let value = auc(&stored.iter().map(score).collect::<Vec<_>>(), &others.iter().map(score).collect::<Vec<_>>());
                row[format!("{}/{dimension}", algorithm.name())] = json!(round(value));
            }
        }
        recognition.push(row);

        // 5. The stored windows kept apart: which one is a damaged query nearest to?
        if stored_count > 1000 {
            continue;
        }
        let held: BTreeSet<&Vec<usize>> = stored.iter().collect();
        let mut queries = Vec::new();
        for _ in 0..20_000 {
            if queries.len() == QUERIES {
                break;
            }
            let original = rng.below(stored_count);
            let mut damaged = stored[original].clone();
            let at = rng.below(WINDOW);
            damaged[at] = (damaged[at] + 1 + rng.below(symbols - 1)) % symbols;
            // A damaged query that is itself a stored window has no one right answer.
            if !held.contains(&damaged) {
                queries.push((original, damaged));
            }
        }
        // When the original ties for best with k - 1 others it earns 1/k:
        // what picking among the tied at random would get. Exact scores
        // tie often and vector scores almost never, so counting a tie as
        // a miss would favour the vectors for their noise.
        let precision = |scores: &dyn Fn(&Vec<usize>, usize) -> f64| {
            let credit: f64 = queries.iter().map(|(original, damaged)| {
                let all: Vec<f64> = (0..stored_count).map(|i| scores(damaged, i)).collect();
                let best = all.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                if all[*original] == best { 1.0 / all.iter().filter(|s| **s == best).count() as f64 } else { 0.0 }
            }).sum();
            round(credit / queries.len().max(1) as f64)
        };
        let mut row = json!({ "windows_stored": stored_count, "queries": queries.len(), "damage": "one event of six replaced by another" });
        row["exact_positions"] = json!(precision(&|q, i| q.iter().zip(&stored[i]).filter(|(a, b)| a == b).count() as f64));
        let stored_histograms: Vec<Vec<f64>> = stored.iter().map(|w| histogram(w)).collect();
        row["exact_counts"] = json!(precision(&|q, i| {
            let (h, g) = (histogram(q), &stored_histograms[i]);
            h.iter().zip(g).map(|(a, b)| a * b).sum::<f64>() / (h.iter().map(|a| a * a).sum::<f64>() * g.iter().map(|a| a * a).sum::<f64>()).sqrt()
        }));
        for dimension in DIMENSIONS {
            for algorithm in Algorithm::ALL {
                let composer = Composer::new(algorithm, dimension).unwrap();
                let alphabet: Vec<Item> = names.iter().map(|n| token(&composer, n)).collect();
                let vectors: Vec<Vec<(f32, f32)>> = stored.iter().map(|w| compose(&composer, &alphabet, w)).collect();
                let cache: std::cell::RefCell<Composed> = Default::default();
                row[format!("{}/{dimension}", algorithm.name())] = json!(precision(&|q, i| {
                    let mut slot = cache.borrow_mut();
                    if slot.as_ref().is_none_or(|(held, _)| held != q) {
                        *slot = Some((q.clone(), compose(&composer, &alphabet, q)));
                    }
                    f64::from(similarity(&slot.as_ref().unwrap().1, &vectors[i]).unwrap())
                }));
            }
        }
        retrieval.push(row);
    }
    json!({
        "label": label,
        "data": "real",
        "events": events,
        "distinct_events": symbols,
        "window": WINDOW,
        "distinct_windows": windows.len(),
        "distinct_windows_that_are_another_reordered": sharing,
        "recognition_auc": recognition,
        "recognition_note": "all stored windows summed into one vector; the score of a window is its similarity to that sum; the figure is the share of (stored, not stored) pairs ranked rightly, 0.5 being chance",
        "nearest_window_precision": retrieval,
        "nearest_window_note": "each stored window kept as its own vector; the query is a stored window with one event replaced; credit 1 when the original is the single nearest, 1/k when it ties for nearest with k - 1 others",
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(args.len() % 3 == 0, "arguments come in threes: LABEL FILE GROUP_FIELD");
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let identities: Vec<String> = Algorithm::ALL.iter().flat_map(|&a| DIMENSIONS.map(|d| Composer::new(a, d).unwrap().identity())).collect();
    let synthetic = synthetic(&mut rng);
    let real: Vec<Value> = args.chunks(3).map(|a| real(&a[0], &a[1], &a[2], &mut rng)).collect();
    let report = json!({
        "what": "composition of token representations (specification F1): order sensitivity, constituent recovery, noise tolerance, capacity, retrieval and cost",
        "identities": identities,
        "seed": "fixed in the source",
        "synthetic": synthetic,
        "real": real,
        "not_measured": "any learned representation; any associative medium or dynamics; sequence completion; generalization. The memory here is a sum and retrieval is an inner product.",
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
