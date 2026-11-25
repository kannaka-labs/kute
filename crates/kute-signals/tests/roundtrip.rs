//! The contract of a signal container: exact means the same bits, bounded
//! means within the bound, and a container that would give anything else
//! is refused. Synthetic data.

use kute_signals::{header, pack, pack_with, unpack, Codec, Mode, Scheme, MAGIC};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn same_bits(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// A meter reading every minute: a slow wave, noise, three decimals.
fn meter(n: usize, seed: u64) -> Vec<f64> {
    let mut rng = Rng(seed);
    (0..n).map(|i| ((230.0 + 4.0 * (i as f64 / 400.0).sin() + rng.unit() * 0.6) * 1000.0).round() / 1000.0).collect()
}

#[test]
fn exact_gives_back_the_same_bits_whatever_the_values() {
    let mut values = meter(50_000, 1);
    // What a real file has in it: gaps, a sentinel, a sign on zero, a
    // reading with more digits than the rest.
    values[10] = f64::NAN;
    values[11] = f64::from_bits(0x7ff8_0000_dead_beef);
    values[500] = f64::INFINITY;
    values[501] = f64::NEG_INFINITY;
    values[900] = -0.0;
    values[901] = 0.0;
    values[2000] = 1.0 / 3.0;
    values[3000] = -9999.25;
    let (container, report) = pack(&values, Mode::Exact).unwrap();
    assert_eq!(&container[..MAGIC.len()], MAGIC);
    let h = &report.header;
    assert_eq!((h.scheme, h.decimals, h.contract.as_str(), h.max_error), (Scheme::Decimal, Some(3), "exact", None));
    assert_eq!(h.max_error_observed, 0.0);
    // NaN twice, the infinities, negative zero and the third: six.
    assert_eq!(h.kept_exactly, 6);
    assert!(same_bits(&unpack(&container, u64::MAX).unwrap(), &values));
    assert_eq!(header(&container).unwrap(), *h);
    // Much smaller than eight bytes a value, since the readings are near each other.
    assert!(report.bits_per_value < 16.0, "{}", report.bits_per_value);
    assert_eq!(report.binary_bytes, 400_000);

    // Whole numbers; and a signal that is a straight line costs almost nothing.
    let counts: Vec<f64> = (0..10_000).map(|i| (i * 7 % 13) as f64).collect();
    let (c, r) = pack(&counts, Mode::Exact).unwrap();
    assert_eq!(r.header.decimals, Some(0));
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &counts));
    let line: Vec<f64> = (0..100_000).map(|i| 1_600_000_000.0 + 60.0 * i as f64).collect();
    let (c, r) = pack(&line, Mode::Exact).unwrap();
    assert!(r.header.delta_order >= 1 && c.len() < 1000, "{} bytes, order {}", c.len(), r.header.delta_order);
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &line));

    // No decimal scale at all: the bytes are stored, still exactly.
    let mut rng = Rng(9);
    let noise: Vec<f64> = (0..20_000).map(|_| f64::from_bits(rng.next())).collect();
    let (c, r) = pack(&noise, Mode::Exact).unwrap();
    assert_eq!(r.header.scheme, Scheme::Bits);
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &noise));
    let smooth: Vec<f64> = (0..20_000).map(|i| (i as f64 / 300.0).sin()).collect();
    let (c, r) = pack(&smooth, Mode::Exact).unwrap();
    assert_eq!(r.header.scheme, Scheme::Bits);
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &smooth));

    // Many readings missing: still a decimal signal, the gaps kept beside it.
    let mut gappy = meter(12_000, 5);
    for v in gappy.iter_mut().step_by(3) {
        *v = f64::NAN;
    }
    let (c, r) = pack(&gappy, Mode::Exact).unwrap();
    assert_eq!((r.header.scheme, r.header.kept_exactly), (Scheme::Decimal, 4000));
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &gappy));

    // Nothing, and one value.
    for tiny in [vec![], vec![42.5], vec![f64::NAN]] {
        let (c, _) = pack(&tiny, Mode::Exact).unwrap();
        assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &tiny));
    }
}

#[test]
fn bounded_keeps_every_value_within_the_bound_and_says_how_close_it_came() {
    let mut rng = Rng(4);
    let mut values: Vec<f64> = (0..80_000).map(|i| 20.0 + 5.0 * (i as f64 / 900.0).sin() + (rng.unit() - 0.5) * 0.02).collect();
    values[7] = f64::NAN;
    values[8] = f64::INFINITY;
    values[9] = 1e300;
    values[10] = -1e300;
    let exact_size = pack(&values, Mode::Exact).unwrap().0.len();
    let mut last = usize::MAX;
    for max_error in [0.0005, 0.01, 0.25] {
        let (container, report) = pack(&values, Mode::Bounded { max_error }).unwrap();
        let back = unpack(&container, u64::MAX).unwrap();
        let mut worst = 0.0f64;
        for (given, got) in values.iter().zip(&back) {
            if given.is_finite() {
                assert!((given - got).abs() <= max_error, "{given} came back {got} with bound {max_error}");
                if given.abs() < 1e200 {
                    worst = worst.max((given - got).abs());
                }
            } else {
                assert_eq!(given.to_bits(), got.to_bits());
            }
        }
        let h = &report.header;
        assert_eq!((h.scheme, h.contract.as_str(), h.max_error), (Scheme::Steps, "bounded", Some(max_error)));
        // The header's figure is the true one.
        assert_eq!(h.max_error_observed, worst);
        assert!(worst > max_error * 0.9, "the step is not being used: {worst}");
        // Too large for any step, or not a number: kept as they were.
        assert_eq!(h.kept_exactly, 4);
        assert_eq!(back[9], 1e300);
        // A looser bound is a smaller container.
        assert!(container.len() < last && container.len() < exact_size, "{} then {}", last, container.len());
        last = container.len();
    }
    // A bound finer than the signal's own resolution: the values are
    // kept as they are, since that is smaller and keeps any bound.
    let counts: Vec<f64> = (0..40_000).map(|i| ((i / 7) * 3 % 17) as f64).collect();
    let exact = pack(&counts, Mode::Exact).unwrap().0.len();
    let (c, r) = pack(&counts, Mode::Bounded { max_error: 0.005 }).unwrap();
    assert_eq!((r.header.scheme, r.header.contract.as_str(), r.header.max_error, r.header.max_error_observed), (Scheme::Decimal, "bounded", Some(0.005), 0.0));
    assert!(c.len() <= exact + 32, "{} against {exact}", c.len());
    assert!(same_bits(&unpack(&c, u64::MAX).unwrap(), &counts));
    // A bound so loose that everything is one step.
    let (c, _) = pack(&values[100..], Mode::Bounded { max_error: 1000.0 }).unwrap();
    assert!(unpack(&c, u64::MAX).unwrap().iter().all(|v| *v == 0.0));
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(pack(&values, Mode::Bounded { max_error: bad }).is_err(), "{bad}");
    }
}

#[test]
fn a_container_that_would_give_other_values_is_refused() {
    let values = meter(5000, 3);
    let (container, _) = pack(&values, Mode::Exact).unwrap();
    assert!(unpack(&container, 4999).is_err(), "the limit on values is not applied");
    assert!(unpack(&container, 5000).is_ok());
    // Every single byte, changed: never a panic, never other values.
    let mut rng = Rng(77);
    let mut refused = 0;
    for at in 0..container.len() {
        let mut bent = container.clone();
        bent[at] ^= 1 << (rng.next() % 8);
        match unpack(&bent, 1 << 20) {
            Err(_) => refused += 1,
            Ok(got) => assert!(same_bits(&got, &values), "byte {at} changed the values and was accepted"),
        }
    }
    assert!(refused > container.len() * 9 / 10, "{refused} of {}", container.len());
    // Cut short anywhere, or with something added.
    for cut in [0, 5, 9, 13, 40, container.len() / 2, container.len() - 1] {
        assert!(unpack(&container[..cut], 1 << 20).is_err(), "cut at {cut}");
    }
    let mut longer = container.clone();
    longer.push(0);
    assert!(unpack(&longer, 1 << 20).is_err());

    // A header that claims a great many values around a few bytes.
    let head_len = u32::from_le_bytes(container[9..13].try_into().unwrap()) as usize;
    let mut forged: serde_json::Value = serde_json::from_slice(&container[13..13 + head_len]).unwrap();
    forged["values"] = serde_json::json!(2_000_000_000u64);
    let head = serde_json::to_vec(&forged).unwrap();
    let mut lying = MAGIC.to_vec();
    lying.extend_from_slice(&(head.len() as u32).to_le_bytes());
    lying.extend_from_slice(&head);
    lying.extend_from_slice(&container[13 + head_len..]);
    assert!(unpack(&lying, u64::MAX).is_err());
    assert!(unpack(&lying, 1 << 20).is_err());
    assert!(unpack(b"KUTE-EV1\n\0\0\0\0", 10).is_err());
}

/// The codec is a choice about bytes, not about values: zlib parts give
/// the same values and the same hash, the header says which it is, and a
/// header that says the wrong one is refused.
#[test]
fn zlib_parts_give_the_same_values_and_the_header_names_them() {
    let values = meter(20_000, 11);
    for mode in [Mode::Exact, Mode::Bounded { max_error: 0.01 }] {
        let (zlib, with_zlib) = pack_with(&values, mode, Codec::Zlib).unwrap();
        assert_eq!(header(&zlib).unwrap().codec, Codec::Zlib);
        let back = unpack(&zlib, values.len() as u64).unwrap();
        let (default, with_default) = pack(&values, mode).unwrap();
        assert_eq!(with_default.header.codec, Codec::preferred());
        assert_eq!(with_zlib.header.values_sha256, with_default.header.values_sha256);
        assert!(same_bits(&back, &unpack(&default, values.len() as u64).unwrap()));

        // The header as text: a Zstandard container names no codec, as in
        // the first release; a zlib one does.
        let head = |container: &[u8]| {
            let length = u32::from_le_bytes(container[MAGIC.len()..MAGIC.len() + 4].try_into().unwrap()) as usize;
            String::from_utf8(container[MAGIC.len() + 4..MAGIC.len() + 4 + length].to_vec()).unwrap()
        };
        assert!(head(&zlib).contains("\"codec\":\"zlib\""));
        if Codec::Zstd.available() {
            assert!(!head(&default).contains("codec"));
            assert!(zlib != default);
        } else {
            assert_eq!(zlib, default);
            assert!(pack_with(&values, mode, Codec::Zstd).unwrap_err().to_string().contains("zstd"));
        }

        // The same parts under a header that claims the other codec.
        let text = head(&zlib).replace("\"codec\":\"zlib\",", "");
        let mut forged = MAGIC.to_vec();
        forged.extend_from_slice(&(text.len() as u32).to_le_bytes());
        forged.extend_from_slice(text.as_bytes());
        forged.extend_from_slice(&zlib[MAGIC.len() + 4 + head(&zlib).len()..]);
        assert!(unpack(&forged, values.len() as u64).is_err());
    }
    assert!(Codec::parse("lzma").is_err());
}
