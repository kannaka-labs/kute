//! `kute-ref`: the `kute` command with the rustbpe reference backend
//! registered, so `--algorithm rustbpe` works and `benchmark --suite`
//! includes the comparison with the native backend.

fn main() -> std::process::ExitCode {
    let mut engine = kute::Engine::new();
    kute_rustbpe::register(engine.registry_mut());
    kute_cli::main_with(engine)
}
