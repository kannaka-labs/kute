//! `kute signals`: numbers go in as text and come back as the same
//! numbers, or within the bound asked for. Synthetic data.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kute-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> (bool, Vec<u8>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kute")).current_dir(dir).args(args).output().unwrap();
    (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let (success, stdout, stderr) = run(dir, args);
    assert!(success, "kute {args:?} failed: {stderr}");
    serde_json::from_slice(&stdout).unwrap()
}

fn numbers(text: &str) -> Vec<f64> {
    text.lines().map(|line| line.parse().unwrap()).collect()
}

#[test]
fn a_column_of_a_file_is_packed_and_read_back() {
    let dir = scratch("signals");
    let mut table = String::from("time;volts;amps\n");
    let mut volts = Vec::new();
    for i in 0..5000u32 {
        // A reading in six is missing, the way the file marks it.
        let v = if i % 6 == 5 { "?".to_string() } else { format!("{:.3}", 230.0 + f64::from(i.wrapping_mul(i).wrapping_mul(7919) % 1000) * 0.001) };
        volts.push(if v == "?" { f64::NAN } else { v.parse().unwrap() });
        table.push_str(&format!("{i};{v};1.5\n"));
    }
    std::fs::write(dir.join("log.csv"), &table).unwrap();

    let packed = ok(&dir, &["signals", "pack", "log.csv", "--column", "2", "--delimiter", ";", "--skip-lines", "1", "--output", "v.kts"]);
    assert_eq!(packed["missing_values"], 833);
    let header = &packed["report"]["header"];
    assert_eq!((header["values"].as_u64(), header["contract"].as_str(), header["decimals"].as_u64()), (Some(5000), Some("exact"), Some(3)));
    assert_eq!(ok(&dir, &["signals", "inspect", "v.kts"]), *header);

    let (success, stdout, _) = run(&dir, &["signals", "unpack", "v.kts"]);
    assert!(success);
    let back = numbers(std::str::from_utf8(&stdout).unwrap());
    assert_eq!(back.len(), volts.len());
    assert!(back.iter().zip(&volts).all(|(a, b)| a == b || (a.is_nan() && b.is_nan())));

    // Within a bound, and the header says how close it came.
    let bounded = ok(&dir, &["signals", "pack", "log.csv", "--column", "2", "--delimiter", ";", "--skip-lines", "1", "--output", "b.kts", "--max-error", "0.1"]);
    let header = &bounded["report"]["header"];
    assert_eq!((header["contract"].as_str(), header["max_error"].as_f64()), (Some("bounded"), Some(0.1)));
    assert!(bounded["report"]["container_bytes"].as_u64() < packed["report"]["container_bytes"].as_u64());
    ok(&dir, &["signals", "unpack", "b.kts", "--output", "b.txt"]);
    let back = numbers(&std::fs::read_to_string(dir.join("b.txt")).unwrap());
    let worst = back.iter().zip(&volts).filter(|(_, b)| !b.is_nan()).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    assert!(worst <= 0.1 && worst == header["max_error_observed"].as_f64().unwrap(), "{worst}");
    assert!(back.iter().zip(&volts).all(|(a, b)| a.is_nan() == b.is_nan()));

    // Very large and very small values are printed with an exponent, and
    // read back as the same numbers.
    std::fs::write(dir.join("wide.txt"), "1e300\n5e-324\n-2.5e-200\n123456.789\n").unwrap();
    ok(&dir, &["signals", "pack", "wide.txt", "--output", "wide.kts"]);
    let (_, stdout, _) = run(&dir, &["signals", "unpack", "wide.kts"]);
    let text = String::from_utf8(stdout).unwrap();
    assert!(text.len() < 60, "{} characters: {text}", text.len());
    assert_eq!(numbers(&text), [1e300, 5e-324, -2.5e-200, 123456.789]);

    // What is refused, and why.
    for (args, why) in [
        (vec!["signals", "pack", "log.csv", "--output", "x.kts"], "is not a number"),
        (vec!["signals", "pack", "log.csv", "--column", "9", "--delimiter", ";", "--output", "x.kts"], "no column 9"),
        (vec!["signals", "pack", "log.csv", "--column", "2", "--delimiter", ";", "--skip-lines", "1", "--max-error", "0", "--output", "x.kts"], "positive"),
        (vec!["signals", "unpack", "v.kts", "--max-values", "4999"], "limit"),
        (vec!["signals", "unpack", "log.csv"], "not a KUTE signal container"),
    ] {
        let (success, _, stderr) = run(&dir, &args);
        assert!(!success && stderr.contains(why), "{args:?}: {stderr}");
    }
    assert!(!dir.join("x.kts").exists(), "a refused pack left a file behind");
    let _ = std::fs::remove_dir_all(&dir);
}
