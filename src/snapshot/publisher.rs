use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;

use super::{
    Snapshot, SnapshotError,
    manifest::{CURRENT_FILE, SNAPSHOTS_DIR},
    opener::validate_snapshot_id,
};

pub struct SnapshotPublisher;

impl SnapshotPublisher {
    pub fn publish(
        index_dir: &Path,
        candidate_dir: &Path,
        snapshot_id: &str,
        snapshot_cache_dir: &Path,
    ) -> Result<PathBuf, SnapshotError> {
        validate_snapshot_id(snapshot_id)?;
        let candidate = Snapshot::open_generation(candidate_dir, snapshot_id, snapshot_cache_dir)?;
        drop(candidate);

        let snapshots_dir = index_dir.join(SNAPSHOTS_DIR);
        fs::create_dir_all(&snapshots_dir)
            .map_err(|source| SnapshotError::io(&snapshots_dir, source))?;
        let generation_path = snapshots_dir.join(snapshot_id);
        if generation_path.exists() {
            match Snapshot::open_generation(&generation_path, snapshot_id, snapshot_cache_dir) {
                Ok(existing) => {
                    drop(existing);
                    fs::remove_dir_all(candidate_dir)
                        .map_err(|source| SnapshotError::io(candidate_dir, source))?;
                }
                Err(_) => {
                    fs::remove_dir_all(&generation_path)
                        .map_err(|source| SnapshotError::io(&generation_path, source))?;
                    fs::rename(candidate_dir, &generation_path)
                        .map_err(|source| SnapshotError::io(candidate_dir, source))?;
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
