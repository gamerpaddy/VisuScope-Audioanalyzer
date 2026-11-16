//! Small file helpers.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// Write a file so that a crash or a power cut leaves either the old or the new content, never half of it: the bytes go to
/// a temporary file next to it, which then replaces the target (`rename` replaces an existing file on Windows too).
pub fn atomic_write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let path = path.as_ref();
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".tmp-{}-{}", std::process::id(), SERIAL.fetch_add(1, Relaxed)));
    let tmp = path.with_file_name(name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes.as_ref())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn replaces_a_file_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("vs_fsutil_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        super::atomic_write(&f, "one").unwrap();
        super::atomic_write(&f, b"two".as_slice()).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
