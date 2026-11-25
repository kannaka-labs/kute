//! What a container says of itself. The hash covers the values and not
//! the header's statements about them, so the statements are checked
//! against each other. Synthetic data.

use kute_signals::{header, pack, unpack, Mode, Scheme, MAGIC};

fn rewrite(container: &[u8], change: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
    let start = MAGIC.len() + 4;
    let length = u32::from_le_bytes(container[MAGIC.len()..start].try_into().unwrap()) as usize;
    let mut head: serde_json::Value = serde_json::from_slice(&container[start..start + length]).unwrap();
    change(&mut head);
    let head = serde_json::to_vec(&head).unwrap();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(head.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&container[start + length..]);
    out
}

fn wave(n: usize) -> Vec<f64> {
    (0..n).map(|i| 230.0 + 4.0 * (i as f64 / 40.0).sin() + (i % 7) as f64 / 3.0).collect()
}

#[test]
fn a_header_that_claims_more_than_its_container_keeps_is_refused() {
    let values = wave(500);
    let (stepped, report) = pack(&values, Mode::Bounded { max_error: 0.01 }).unwrap();
    assert_eq!(report.header.scheme, Scheme::Steps);
    assert!(report.header.max_error_observed > 0.0);
    let n = values.len() as u64;

    type Change = Box<dyn Fn(&mut serde_json::Value)>;
    let lies: Vec<(&str, Change)> = vec![
        ("values in steps called exact", Box::new(|h| {
            h["contract"] = "exact".into();
            h.as_object_mut().unwrap().remove("max_error");
            h["max_error_observed"] = 0.0.into();
        })),
        ("exact with an error", Box::new(|h| {
            h["contract"] = "exact".into();
            h.as_object_mut().unwrap().remove("max_error");
        })),
        ("a contract with no name", Box::new(|h| h["contract"] = "anything".into())),
        ("bounded with no bound", Box::new(|h| {
            h.as_object_mut().unwrap().remove("max_error");
        })),
        ("a bound of zero", Box::new(|h| h["max_error"] = 0.0.into())),
        ("an error below zero", Box::new(|h| h["max_error_observed"] = (-1.0).into())),
        ("an error past the bound", Box::new(|h| h["max_error_observed"] = 0.02.into())),
        ("steps and decimals at once", Box::new(|h| h["decimals"] = 3.into())),
    ];
    for (what, lie) in &lies {
        let forged = rewrite(&stepped, lie);
        assert!(unpack(&forged, n).is_err(), "{what}: unpack");
        assert!(header(&forged).is_err(), "{what}: header");
    }
    // The untouched container is still read.
    assert_eq!(unpack(&stepped, n).unwrap().len(), values.len());
}

#[test]
fn a_bound_too_large_to_step_by_is_kept_by_exact_values() {
    let values = wave(200);
    for max_error in [8.99e307, 1e308, f64::MAX] {
        let (container, report) = pack(&values, Mode::Bounded { max_error }).unwrap();
        assert_eq!(report.header.contract, "bounded");
        let back = unpack(&container, values.len() as u64).unwrap();
        assert!(values.iter().zip(&back).all(|(a, b)| (a - b).abs() <= max_error));
    }
    assert!(pack(&values, Mode::Bounded { max_error: f64::INFINITY }).is_err());
}
