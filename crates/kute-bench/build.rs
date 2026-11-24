use std::process::Command;

// Record the compiler version so benchmark reports can state it.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KUTE_BENCH_RUSTC={version}");
    println!("cargo:rustc-env=KUTE_BENCH_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());
    println!("cargo:rerun-if-changed=build.rs");
}
