//! Composition of token representations: what each method keeps, what it
//! loses, and that its identity names everything that fixes the vectors.
//! Synthetic sequences.

use kute_hrm::compose::{similarity, Algorithm, Composer, Item, PORTABLE_TOLERANCE};

fn items(composer: &Composer, names: &[&str]) -> Vec<Item> {
    names.iter().map(|n| composer.item(n)).collect()
}

fn refs(items: &[Item]) -> Vec<&[(f32, f32)]> {
    items.iter().map(|i| i.as_slice()).collect()
}

const WORDS: [&str; 8] = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"];

#[test]
fn order_is_lost_by_the_sum_and_kept_by_the_other_two() {
    for algorithm in Algorithm::ALL {
        let composer = Composer::new(algorithm, 2048).unwrap();
        let forward = items(&composer, &WORDS);
        let mut backward = forward.clone();
        backward.reverse();
        let a = composer.compose(&refs(&forward)).unwrap();
        let b = composer.compose(&refs(&backward)).unwrap();
        assert!((similarity(&a, &a).unwrap() - 1.0).abs() < 1e-4);
        let s = similarity(&a, &b).unwrap();
        match algorithm {
            Algorithm::Bundle => assert!(s > 0.999, "{s}"),
            _ => assert!(s.abs() < 0.15, "{algorithm:?}: {s}"),
        }
        // One token changed: the sums stay close, the chain does not.
        let mut one_off = forward.clone();
        one_off[3] = composer.item("zulu");
        let s = similarity(&a, &composer.compose(&refs(&one_off)).unwrap()).unwrap();
        match algorithm {
            Algorithm::BindChain => assert!(s.abs() < 0.15, "{s}"),
            _ => assert!((0.7..0.95).contains(&s), "{algorithm:?}: {s}"),
        }
    }
}

#[test]
fn a_constituent_is_found_again_where_the_method_allows_it() {
    let names: Vec<String> = (0..64).map(|i| format!("token-{i}")).collect();
    for algorithm in Algorithm::ALL {
        let composer = Composer::new(algorithm, 1024).unwrap();
        let alphabet: Vec<Item> = names.iter().map(|n| composer.item(n)).collect();
        let sequence = [5usize, 17, 5, 40, 2, 63, 31, 8];
        let parts: Vec<&[(f32, f32)]> = sequence.iter().map(|&i| alphabet[i].as_slice()).collect();
        let composed = composer.compose(&parts).unwrap();
        for (position, &expected) in sequence.iter().enumerate() {
            let known: Vec<(usize, &[(f32, f32)])> = sequence.iter().enumerate().filter(|(p, _)| *p != position).map(|(p, &i)| (p, alphabet[i].as_slice())).collect();
            let found = composer.recover(&composed, position, &known, &alphabet).unwrap();
            match algorithm {
                // The sum knows which tokens, not where: it finds one of them.
                Algorithm::Bundle => assert!(sequence.contains(&found)),
                _ => assert_eq!(found, expected, "{algorithm:?} at {position}"),
            }
        }
        // The chain gives nothing back without the others.
        if algorithm == Algorithm::BindChain {
            let hits = (0..sequence.len()).filter(|&p| composer.recover(&composed, p, &[], &alphabet).unwrap() == sequence[p]).count();
            assert!(hits <= 2, "{hits} of 8 found with nothing known");
        }
    }
}

#[test]
fn the_identity_names_everything_that_fixes_the_vectors() {
    let a = Composer::new(Algorithm::PermuteBundle, 512).unwrap();
    let id = a.identity();
    for part in ["permute-bundle", "@0.1.0", "dimension=512", "rotation=1-per-position", "kannaka:hrm:phase-hash@0.1.0"] {
        assert!(id.contains(part), "{id} lacks {part}");
    }
    let mut ids: Vec<String> = Algorithm::ALL.iter().flat_map(|&g| [256, 512].map(|d| Composer::new(g, d).unwrap().identity())).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 6);
    // Vectors of different dimensions are not compared, and bad input is refused.
    let b = Composer::new(Algorithm::PermuteBundle, 256).unwrap();
    let (x, y) = (a.compose(&[&a.item("x")]).unwrap(), b.compose(&[&b.item("x")]).unwrap());
    assert!(similarity(&x, &y).is_err());
    assert!(a.compose(&[]).is_err());
    assert!(a.compose(&[&b.item("x")]).is_err());
    assert!(a.recover(&y, 0, &[], &[a.item("x")]).is_err());
    assert!(a.recover(&x, 0, &[], &[]).is_err());
    assert!(Composer::new(Algorithm::Bundle, 0).is_err());
}

/// Values recorded on one machine. Another platform must land within the
/// portable tolerance, which is what the tolerance is for.
#[test]
fn similarities_agree_with_the_recorded_values_within_the_portable_tolerance() {
    let recorded = [RECORDED_BUNDLE, RECORDED_PERMUTE, RECORDED_CHAIN];
    for (algorithm, expected) in Algorithm::ALL.into_iter().zip(recorded) {
        let composer = Composer::new(algorithm, 1024).unwrap();
        let first = items(&composer, &WORDS[..6]);
        let second = items(&composer, &["alpha", "bravo", "delta", "charlie", "echo", "kilo"]);
        let s = similarity(&composer.compose(&refs(&first)).unwrap(), &composer.compose(&refs(&second)).unwrap()).unwrap();
        assert!((s - expected).abs() <= PORTABLE_TOLERANCE, "{algorithm:?}: {s} against the recorded {expected}");
    }
}

const RECORDED_BUNDLE: f32 = 0.836_433_77;
const RECORDED_PERMUTE: f32 = 0.490_797_94;
const RECORDED_CHAIN: f32 = 0.010_975_849;
