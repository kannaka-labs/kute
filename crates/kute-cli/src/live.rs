//! `kute live`: windowed discovery over JSON Lines arriving on a file or
//! on standard input.
//!
//! One JSON object per line goes in; one JSON object per line comes out:
//! `{"window": ...}` for each window that closes and `{"summary": ...}` at
//! the end. Reading is synchronous, so a writer on a pipe is slowed to the
//! speed of counting and nothing queues beyond the watch's own bounded
//! buffer.

use kute_discover::watch::{Watch, WatchParams};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

/// Longest line read as one event.
const MAX_LINE_BYTES: usize = 1 << 20;

pub struct Options {
    /// The file to read; standard input when absent.
    pub input: Option<PathBuf>,
    pub symbol_field: String,
    pub time_field: String,
    pub group_field: Option<String>,
    pub params: WatchParams,
    /// Where state is saved, and resumed from when `resume` is set.
    pub checkpoint: Option<PathBuf>,
    /// Save after this many window reports.
    pub checkpoint_every: u64,
    pub resume: bool,
    /// Count and pass over lines that cannot be read as events, instead
    /// of stopping.
    pub skip_bad_lines: bool,
}

fn invalid(why: String) -> kute::KuteError {
    kute::KuteError::InvalidInput(why)
}

/// The three fields of one line, or why it is not an event.
fn fields<'a>(line: &'a [u8], options: &Options, scratch: &'a mut String) -> Result<(&'a str, String, i64), String> {
    let value: Value = serde_json::from_slice(line).map_err(|e| format!("not JSON: {e}"))?;
    let object = value.as_object().ok_or("not a JSON object")?;
    let symbol = match object.get(&options.symbol_field) {
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(format!("field {:?} is not a string", options.symbol_field)),
        None => return Err(format!("no field {:?}", options.symbol_field)),
    };
    let time = match object.get(&options.time_field) {
        Some(Value::Number(n)) => n.as_i64().ok_or_else(|| format!("field {:?} is not a whole number in 64 bits", options.time_field))?,
        Some(_) => return Err(format!("field {:?} is not a number", options.time_field)),
        None => return Err(format!("no field {:?}", options.time_field)),
    };
    scratch.clear();
    if let Some(field) = &options.group_field {
        match object.get(field) {
            // The JSON text, so that 1 and "1" are two entities.
            Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => scratch.push_str(&v.to_string()),
            Some(_) => return Err(format!("field {field:?} is not a string, a number or a boolean")),
            None => return Err(format!("no field {field:?}")),
        }
    }
    Ok((scratch.as_str(), symbol, time))
}

/// The watch's state with the reader's position, written beside the old
/// file and moved over it, so that a crash leaves one whole checkpoint.
fn save(path: &Path, watch: &Watch, lines: u64, bad: u64) -> kute::Result<()> {
    let state: Value = serde_json::from_slice(&watch.checkpoint()?)?;
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    std::fs::write(&partial, serde_json::to_vec(&json!({ "kute_live_checkpoint": 1, "lines": lines, "bad_lines": bad, "watch": state }))?)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

pub fn run(options: Options) -> kute::Result<()> {
    let started = std::time::Instant::now();
    let (mut watch, skip, mut bad) = match (&options.checkpoint, options.resume) {
        (Some(path), true) => {
            let saved: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            if saved["kute_live_checkpoint"] != 1 {
                return Err(invalid(format!("{} is not a live checkpoint", path.display())));
            }
            let watch = Watch::restore(&serde_json::to_vec(&saved["watch"])?)?;
            if watch.params() != &options.params {
                return Err(kute::KuteError::InvalidConfig("the checkpoint was made with other parameters; resume with the same ones".into()));
            }
            let number = |key: &str| saved[key].as_u64().ok_or_else(|| invalid(format!("the checkpoint has no {key}")));
            (watch, number("lines")?, number("bad_lines")?)
        }
        (None, true) => return Err(kute::KuteError::InvalidConfig("--resume needs --checkpoint".into())),
        _ => (Watch::new(options.params.clone())?, 0, 0),
    };
    if options.checkpoint_every == 0 {
        return Err(kute::KuteError::InvalidConfig("--checkpoint-every must be at least 1".into()));
    }
    let mut reader: Box<dyn BufRead> = match &options.input {
        Some(path) => Box::new(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?)),
        None => Box::new(std::io::stdin().lock()),
    };
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut line: Vec<u8> = Vec::new();
    let mut scratch = String::new();
    // Lines that are not blank, counted from the start of the input.
    let mut lines: u64 = 0;
    let mut since_save: u64 = 0;
    let mut windows: u64 = 0;
    let mut checkpoints: u64 = 0;
    let resumed_from = skip;
    loop {
        line.clear();
        // At most one byte past the limit is read for one line.
        let read = Read::take(&mut reader, MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        lines += 1;
        if lines <= skip {
            continue;
        }
        let event = if line.len() > MAX_LINE_BYTES && line.last() != Some(&b'\n') { Err("longer than 1 MiB".to_string()) } else { fields(&line, &options, &mut scratch) };
        let reports = match event {
            Ok((entity, symbol, time)) => watch.observe(entity, &symbol, time),
            Err(why) if options.skip_bad_lines => {
                // What is left of an overlong line is read as lines of its
                // own and, not being JSON objects, counted as bad too.
                let _ = why;
                bad += 1;
                continue;
            }
            Err(why) => return Err(invalid(format!("line {lines}: {why}"))),
        };
        if reports.is_empty() {
            continue;
        }
        for report in &reports {
            writeln!(out, "{}", serde_json::to_string(&json!({ "window": report }))?)?;
        }
        out.flush()?;
        windows += reports.len() as u64;
        since_save += reports.len() as u64;
        if let Some(path) = options.checkpoint.as_ref().filter(|_| since_save >= options.checkpoint_every) {
            save(path, &watch, lines, bad)?;
            since_save = 0;
            checkpoints += 1;
        }
    }
    if lines < skip {
        return Err(invalid(format!("the checkpoint is {skip} lines into its input and this input has {lines}")));
    }
    let bound = watch.params().state_bytes_bound();
    let (reports, totals) = watch.finish();
    for report in &reports {
        writeln!(out, "{}", serde_json::to_string(&json!({ "window": report }))?)?;
    }
    windows += reports.len() as u64;
    let seconds = started.elapsed().as_secs_f64();
    let read_now = lines - resumed_from;
    let summary = json!({
        "lines": lines,
        "lines_skipped_on_resume": resumed_from,
        "bad_lines": bad,
        "totals": totals,
        "windows_reported_this_run": windows,
        "checkpoints_written": checkpoints,
        "seconds": (seconds * 1000.0).round() / 1000.0,
        "events_per_second": if seconds > 0.0 { (read_now as f64 / seconds).round() } else { 0.0 },
        "state_bytes_bound": bound,
    });
    writeln!(out, "{}", serde_json::to_string(&json!({ "summary": summary }))?)?;
    out.flush()?;
    Ok(())
}
