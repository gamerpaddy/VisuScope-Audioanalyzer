//! Native file dialogs on a worker thread, so the UI never blocks while one is open.
//! Every dialog starts in the folder of the last file picked or saved (remembered across runs).

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Result of a dialog that is still open (or finished and not yet collected).
#[derive(Clone, Default)]
pub struct Pending(Arc<Mutex<Option<Option<PathBuf>>>>);

impl Pending {
    /// `Some(Some(path))` once a file was chosen, `Some(None)` if cancelled.
    pub fn take(&self) -> Option<Option<PathBuf>> {
        self.0.try_lock().and_then(|mut g| g.take())
    }
}

static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn store_file() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("visuscope").join("last_dir.txt")
}

/// Folder the next dialog opens in.
fn start_dir() -> PathBuf {
    let mut g = LAST_DIR.lock();
    if g.is_none() {
        *g = std::fs::read_to_string(store_file()).ok().map(|s| PathBuf::from(s.trim())).filter(|p| p.is_dir());
    }
    g.clone().unwrap_or_else(super::docs_dir)
}

/// The remembered folder, if there is one.
pub fn last_dir() -> Option<PathBuf> {
    start_dir();
    LAST_DIR.lock().clone()
}

/// Start in Documents again.
pub fn forget_dir() {
    *LAST_DIR.lock() = None;
    std::fs::remove_file(store_file()).ok();
}

fn remember(path: &Path) {
    let dir = if path.is_dir() { Some(path) } else { path.parent() };
    let Some(dir) = dir.filter(|d| d.is_dir()) else { return };
    *LAST_DIR.lock() = Some(dir.to_path_buf());
    let f = store_file();
    if let Some(p) = f.parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(f, dir.to_string_lossy().as_bytes()).ok();
}

fn spawn(f: impl FnOnce() -> Option<PathBuf> + Send + 'static) -> Pending {
    let p = Pending::default();
    let slot = p.0.clone();
    std::thread::Builder::new()
        .name("dialog".into())
        .spawn(move || {
            let r = f();
            if let Some(path) = &r {
                remember(path);
            }
            *slot.lock() = Some(r);
        })
        .ok();
    p
}

pub fn open(title: &'static str, name: &'static str, exts: &'static [&'static str]) -> Pending {
    let dir = start_dir();
    spawn(move || rfd::FileDialog::new().set_title(title).set_directory(dir).add_filter(name, exts).pick_file())
}

pub fn save(title: &'static str, name: &'static str, exts: &'static [&'static str], file: String) -> Pending {
    let dir = start_dir();
    spawn(move || rfd::FileDialog::new().set_title(title).set_directory(dir).set_file_name(file).add_filter(name, exts).save_file())
}

/// Several files at once.
#[derive(Clone, Default)]
pub struct PendingMany(Arc<Mutex<Option<Vec<PathBuf>>>>);

impl PendingMany {
    pub fn take(&self) -> Option<Vec<PathBuf>> {
        self.0.try_lock().and_then(|mut g| g.take())
    }
}

pub fn open_many(title: &'static str, name: &'static str, exts: &'static [&'static str]) -> PendingMany {
    let dir = start_dir();
    let p = PendingMany::default();
    let slot = p.0.clone();
    std::thread::Builder::new()
        .name("dialog".into())
        .spawn(move || {
            let r = rfd::FileDialog::new().set_title(title).set_directory(dir).add_filter(name, exts).pick_files().unwrap_or_default();
            if let Some(first) = r.first() {
                remember(first);
            }
            *slot.lock() = Some(r);
        })
        .ok();
    p
}

pub fn pick_folder(title: &'static str) -> Pending {
    let dir = start_dir();
    spawn(move || rfd::FileDialog::new().set_title(title).set_directory(dir).pick_folder())
}
