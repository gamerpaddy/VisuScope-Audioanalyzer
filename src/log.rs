//! Tiny append-only event log at `%APPDATA%\visuscope\visuscope.log` (audio events, hangs, panics).

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("visuscope").join("visuscope.log")
}

/// Call once at start: begins the uptime clock and trims an oversized log.
pub fn init() {
    START.get_or_init(Instant::now);
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&p).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&p);
    }
    log("---- start ----");
}

pub fn log(msg: &str) {
    let up = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path()) {
        let _ = writeln!(f, "[unix {wall} +{up:.2}s] {msg}");
    }
}
