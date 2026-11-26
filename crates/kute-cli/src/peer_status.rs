//! What a running `kute peer serve` says about itself, for someone at the
//! same machine to read.
//!
//! The peer and the HTTP service are separate processes, and the peer
//! opens no second port for this. With `--status-file` it writes a small
//! JSON file: when it starts listening, when a connection is refused,
//! when an exchange begins, once a second while it runs, and when it
//! ends. `kute serve --peer-status FILE` reads that file when asked
//! (`GET /v1/peer`), and Studio shows it.
//!
//! A file cannot say that its writer is alive. What it can say is when it
//! was last written, and a peer that is running writes every second, so
//! a file that has not been written for several seconds while claiming
//! to be running is reported as stale and not as running.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The number in `kute_peer_status`. A reader refuses any other.
pub const FORMAT: u64 = 1;
/// How often a running peer rewrites the file.
pub const HEARTBEAT: Duration = Duration::from_secs(1);
/// The largest status file a reader takes.
pub const MAX_BYTES: u64 = 65_536;

pub const WAITING: &str = "waiting for a connection";
pub const EXCHANGE: &str = "in an exchange";
pub const ENDED: &str = "ended";

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

struct Writer {
    path: PathBuf,
    value: Mutex<Value>,
    /// Held for the whole of a write. The beat and a change of state
    /// write from two threads to one temporary name: without this, one
    /// renames the other's file away, and the write that fails may be
    /// the one saying the peer ended, which nothing writes again.
    writing: Mutex<()>,
}

static WRITER: OnceLock<Arc<Writer>> = OnceLock::new();

impl Writer {
    /// Write the whole file beside the old one, then put it in its place,
    /// so a reader sees one state or the next and never half of one.
    fn write(&self) -> std::io::Result<()> {
        let _one_at_a_time = self.writing.lock().unwrap_or_else(|p| p.into_inner());
        let mut value = self.value.lock().unwrap_or_else(|p| p.into_inner()).clone();
        value["written"] = json!(now());
        let bytes = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".writing-{}", std::process::id()));
        let beside = self.path.with_file_name(name);
        std::fs::write(&beside, bytes)?;
        std::fs::rename(&beside, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&beside);
        })
    }
}

/// Begin writing status to `path`, starting with `first`. A path that
/// cannot be written is the operator's mistake, told now.
pub fn start(path: &Path, mut first: Value) -> kute::Result<()> {
    first["kute_peer_status"] = json!(FORMAT);
    first["heartbeat_seconds"] = json!(HEARTBEAT.as_secs());
    first["started"] = json!(now());
    first["since"] = json!(now());
    let writer = Arc::new(Writer { path: path.to_path_buf(), value: Mutex::new(first), writing: Mutex::new(()) });
    writer.write().map_err(|e| kute::KuteError::InvalidConfig(format!("the status file {} cannot be written: {e}", path.display())))?;
    if WRITER.set(writer.clone()).is_ok() {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(HEARTBEAT);
                // A write that fails (the file is open elsewhere, the disk
                // is full) is tried again at the next beat.
                let _ = writer.write();
            }
        });
    }
    Ok(())
}

/// Change what the next write says. Nothing is written now: counts that
/// move with every message are picked up by the next beat.
pub fn set(change: impl FnOnce(&mut Value)) {
    if let Some(writer) = WRITER.get() {
        change(&mut writer.value.lock().unwrap_or_else(|p| p.into_inner()));
    }
}

/// Change the state and write at once.
pub fn enter(state: &str, change: impl FnOnce(&mut Value)) {
    if let Some(writer) = WRITER.get() {
        {
            let mut value = writer.value.lock().unwrap_or_else(|p| p.into_inner());
            value["state"] = json!(state);
            value["since"] = json!(now());
            change(&mut value);
        }
        let _ = writer.write();
    }
}

/// The peer is ending, this way.
pub fn end(result: &kute::Result<()>) {
    enter(ENDED, |value| {
        value["ended"] = match result {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        };
    });
}

/// Read a status file for the HTTP service. Never an error: a file that
/// is missing, too large or not a status is an answer about the peer.
pub fn read(path: &Path) -> Value {
    let absent = |why: &str| json!({ "present": false, "why": why, "file": path });
    let Ok(metadata) = std::fs::metadata(path) else {
        return absent("no status file is there: no peer has been started with --status-file naming it, or the file was removed");
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return absent("what is there is not a status file: it is not a plain file of at most 64 KiB");
    }
    let Some(status) = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).filter(|v| v["kute_peer_status"] == json!(FORMAT) && v["state"].is_string()) else {
        return absent("the file could not be read as a peer status of the kind this version reads");
    };
    let read_at = now();
    let written = status["written"].as_u64().unwrap_or(0);
    let age = read_at.saturating_sub(written);
    let beat = status["heartbeat_seconds"].as_u64().unwrap_or(1).clamp(1, 60);
    let ended = status["state"] == ENDED;
    // Writer and reader share a clock. A time of writing later than now
    // by more than a beat is not a fresh file but one whose time cannot
    // be believed, and it is not taken as proof of a running peer.
    let from_the_future = written > read_at + beat;
    let stale = !ended && (age > 3 * beat || from_the_future);
    json!({
        "present": true,
        "file": path,
        "read_at": read_at,
        "age_seconds": age,
        "written_later_than_now": from_the_future,
        "stale": stale,
        "status": status,
        "means": if ended {
            "the peer wrote that it had ended. kute peer serve answers one connection and ends; nothing is running."
        } else if stale {
            "the file says a peer is running, but it has not been written for longer than three of its beats, or says it was written later than now. The peer has stopped without saying so, or cannot write, or the file is not a peer's. Nothing here shows a running peer."
        } else {
            "a process wrote this within the last few seconds. It is that process's own account; this service did not connect to it."
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The beat and a change of state write from two threads. Whatever
    /// order they finish in, a reader finds a whole status, and the file
    /// left at the end is the last state entered.
    #[test]
    fn two_writers_at_once_leave_a_whole_file_saying_the_last_state() {
        let dir = std::env::temp_dir().join(format!("kute-peer-status-writers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("peer.json");
        // Large enough that a write takes long enough to be met halfway.
        let writer = Arc::new(Writer { path: path.clone(), value: Mutex::new(json!({ "kute_peer_status": FORMAT, "state": WAITING, "pad": "x".repeat(40_000) })), writing: Mutex::new(()) });
        writer.write().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let beats: Vec<_> = (0..3)
            .map(|_| {
                let (writer, stop) = (writer.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = writer.write();
                    }
                })
            })
            .collect();
        let reader = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let (mut whole, mut broken) = (0u64, 0u64);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if serde_json::from_slice::<Value>(&bytes).is_ok() { whole += 1 } else { broken += 1 }
                    }
                }
                (whole, broken)
            })
        };
        let mut lost = 0;
        let mut failed: Vec<String> = Vec::new();
        for round in 0..60 {
            let state = format!("state {round}");
            writer.value.lock().unwrap()["state"] = json!(state);
            if let Err(e) = writer.write() { failed.push(e.to_string()); }
            std::thread::sleep(Duration::from_millis(1));
            stop.store(round == 59, std::sync::atomic::Ordering::Relaxed);
        }
        for beat in beats {
            beat.join().unwrap();
        }
        let (whole, broken) = reader.join().unwrap();
        let last: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if last["state"] != json!("state 59") {
            lost += 1;
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n.to_string_lossy().contains("writing")).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(whole > 0);
        assert_eq!((broken, lost, left.len(), failed.len()), (0, 0, 0, 0), "state writes that failed: {} (first: {:?}); reads that were not a whole status: {broken} of {}; last state lost: {lost}; temp files left: {left:?}", failed.len(), failed.first(), whole + broken);
    }
}
