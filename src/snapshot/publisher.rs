use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;

use super::{
    Snapshot, SnapshotError,
    lock::IndexLock,
    manifest::{CURRENT_FILE, SNAPSHOTS_DIR},
    opener::validate_snapshot_id,
};

pub struct SnapshotPublisher;

impl SnapshotPublisher {
    /// Publishes `candidate_dir` as generation `snapshot_id` and points CURRENT at it.
    ///
    /// An existing valid generation with the same ID is kept and the candidate discarded.
    /// Holds the index lock for the whole publication and cleanup.
    pub fn publish(
        index_dir: &Path,
        candidate_dir: &Path,
        snapshot_id: &str,
        snapshot_cache_dir: &Path,
    ) -> Result<PathBuf, SnapshotError> {
        let lock = IndexLock::acquire(index_dir)?;
        Self::publish_locked(
            &lock,
            index_dir,
            candidate_dir,
            snapshot_id,
            snapshot_cache_dir,
            false,
        )
    }

    /// Like [`Self::publish`], for callers already holding the index lock. With
    /// `replace_existing`, an existing same-ID generation is replaced by the candidate.
    pub(crate) fn publish_locked(
        _lock: &IndexLock,
        index_dir: &Path,
        candidate_dir: &Path,
        snapshot_id: &str,
        snapshot_cache_dir: &Path,
        replace_existing: bool,
    ) -> Result<PathBuf, SnapshotError> {
        validate_snapshot_id(snapshot_id)?;
        let candidate = Snapshot::open_generation(candidate_dir, snapshot_id, snapshot_cache_dir)?;
        drop(candidate);

        let snapshots_dir = index_dir.join(SNAPSHOTS_DIR);
        fs::create_dir_all(&snapshots_dir)
            .map_err(|source| SnapshotError::io(&snapshots_dir, source))?;
        let generation_path = snapshots_dir.join(snapshot_id);
        if generation_path.exists() {
            let keep_existing = !replace_existing
                && Snapshot::open_generation(&generation_path, snapshot_id, snapshot_cache_dir)
                    .is_ok();
            if keep_existing {
                fs::remove_dir_all(candidate_dir)
                    .map_err(|source| SnapshotError::io(candidate_dir, source))?;
            } else {
                let replaced = index_dir.join(format!(".replaced-{snapshot_id}"));
                replace_generation(&generation_path, candidate_dir, &replaced)?;
                if let Err(error) = fs::remove_dir_all(&replaced) {
                    tracing::debug!(path = %replaced.display(), %error, "replaced snapshot remains in use");
                }
            }
        } else {
            fs::rename(candidate_dir, &generation_path)
                .map_err(|source| SnapshotError::io(candidate_dir, source))?;
        }

        let current_path = index_dir.join(CURRENT_FILE);
        let mut current = AtomicWriteFile::open(&current_path)
            .map_err(|source| SnapshotError::io(&current_path, source))?;
        writeln!(current, "{snapshot_id}")
            .map_err(|source| SnapshotError::io(&current_path, source))?;
        current
            .commit()
            .map_err(|source| SnapshotError::io(&current_path, source))?;
        cleanup_obsolete(&snapshots_dir, snapshot_id);
        Ok(generation_path)
    }
}

/// Moves `generation` aside to `replaced` and renames `candidate` into its place,
/// restoring the previous generation if the candidate cannot be moved in.
fn replace_generation(
    generation: &Path,
    candidate: &Path,
    replaced: &Path,
) -> Result<(), SnapshotError> {
    if replaced.exists() {
        fs::remove_dir_all(replaced).map_err(|source| SnapshotError::io(replaced, source))?;
    }
    fs::rename(generation, replaced).map_err(|source| SnapshotError::io(generation, source))?;
    if let Err(source) = fs::rename(candidate, generation) {
        if let Err(error) = fs::rename(replaced, generation) {
            tracing::error!(
                path = %replaced.display(),
                %error,
                "failed to restore the previous snapshot generation"
            );
        }
        return Err(SnapshotError::io(candidate, source));
    }
    Ok(())
}

fn cleanup_obsolete(snapshots_dir: &Path, current_id: &str) {
    let Ok(entries) = fs::read_dir(snapshots_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == current_id || validate_snapshot_id(name).is_err() {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(entry.path()) {
            tracing::debug!(path = %entry.path().display(), %error, "snapshot remains in use");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::replace_generation;

    #[test]
    fn replace_generation_swaps_and_restores_on_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let generation = temporary.path().join("generation");
        let candidate = temporary.path().join("candidate");
        let replaced = temporary.path().join("replaced");
        fs::create_dir_all(&generation).unwrap();
        fs::write(generation.join("marker"), b"old").unwrap();
        fs::create_dir_all(&candidate).unwrap();
        fs::write(candidate.join("marker"), b"new").unwrap();

        replace_generation(&generation, &candidate, &replaced).unwrap();
        assert_eq!(fs::read(generation.join("marker")).unwrap(), b"new");
        assert_eq!(fs::read(replaced.join("marker")).unwrap(), b"old");
        assert!(!candidate.exists());

        let missing_candidate = temporary.path().join("missing");
        assert!(replace_generation(&generation, &missing_candidate, &replaced).is_err());
        assert_eq!(fs::read(generation.join("marker")).unwrap(), b"new");
        assert!(!replaced.exists());
    }
}
