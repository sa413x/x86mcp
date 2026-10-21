use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

use fs2::FileExt;

use super::SnapshotError;

pub(crate) const INDEX_LOCK_FILE: &str = ".x86mcp-index.lock";

/// Exclusive per-index lock serializing snapshot builds, publication, cleanup
/// and `setup` replacing the installed data.
///
/// The lock file lives beside the index directory, not inside it, so moving or
/// replacing the whole directory cannot detach a waiting process from it.
/// Released when dropped. A contended lock is waited for, never skipped.
pub struct IndexLock {
    file: File,
}

impl IndexLock {
    pub fn acquire(index_dir: &Path) -> Result<Self, SnapshotError> {
        let lock_dir = index_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(lock_dir).map_err(|source| SnapshotError::io(lock_dir, source))?;
        let path = lock_dir.join(INDEX_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| SnapshotError::io(&path, source))?;
        if let Err(error) = file.try_lock_exclusive() {
            if error.raw_os_error() != fs2::lock_contended_error().raw_os_error() {
                return Err(SnapshotError::io(&path, error));
            }
            tracing::info!(
                path = %path.display(),
                "waiting for another index build or publication to release the index lock"
            );
            file.lock_exclusive()
                .map_err(|source| SnapshotError::io(&path, source))?;
        }
        Ok(Self { file })
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
