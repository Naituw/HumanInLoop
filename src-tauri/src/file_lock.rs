//! Cross-platform advisory file locks used for process singletons and serialized state writes.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn exclusive(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }

    pub fn try_exclusive(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if lock_is_contended(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn lock_is_contended(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }

    // fs2 forwards Win32 lock errors without mapping ERROR_LOCK_VIOLATION to
    // WouldBlock. Normalize both contention codes so callers have one API.
    #[cfg(windows)]
    return matches!(error.raw_os_error(), Some(32 | 33));

    #[cfg(not(windows))]
    false
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_lock_blocks_a_second_owner_and_recovers_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.lock");
        let first = FileLock::try_exclusive(&path).unwrap().unwrap();
        assert!(FileLock::try_exclusive(&path).unwrap().is_none());
        drop(first);
        assert!(FileLock::try_exclusive(&path).unwrap().is_some());
    }
}
