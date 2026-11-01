//! Native file dialogs on a worker thread, so the UI never blocks while one is open.

use parking_lot::Mutex;
use std::path::PathBuf;
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

fn spawn(f: impl FnOnce() -> Option<PathBuf> + Send + 'static) -> Pending {
    let p = Pending::default();
    let slot = p.0.clone();
    std::thread::Builder::new()
        .name("dialog".into())
        .spawn(move || {
            let r = f();
            *slot.lock() = Some(r);
        })
        .ok();
    p
}

pub fn open(title: &'static str, name: &'static str, exts: &'static [&'static str]) -> Pending {
    let dir = super::docs_dir();
    spawn(move || rfd::FileDialog::new().set_title(title).set_directory(dir).add_filter(name, exts).pick_file())
}

pub fn save(title: &'static str, name: &'static str, exts: &'static [&'static str], file: String) -> Pending {
    let dir = super::docs_dir();
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
    let dir = super::docs_dir();
    let p = PendingMany::default();
    let slot = p.0.clone();
    std::thread::Builder::new()
        .name("dialog".into())
        .spawn(move || {
            let r = rfd::FileDialog::new().set_title(title).set_directory(dir).add_filter(name, exts).pick_files().unwrap_or_default();
            *slot.lock() = Some(r);
        })
        .ok();
    p
}

pub fn pick_folder(title: &'static str) -> Pending {
    let dir = super::docs_dir();
    spawn(move || rfd::FileDialog::new().set_title(title).set_directory(dir).pick_folder())
}
