use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use parking_lot::Mutex;

use crate::{
    config::AppConfig,
    corpus::CorpusManifest,
    index::{FastEmbedder, model_snapshot_artifacts},
};

use super::{
    ArchiveFreshness, Snapshot, SnapshotError, SnapshotManifest, SnapshotState, SnapshotStatus,
    manifest::{CURRENT_FILE, blake3_file, ensure_relative_artifact, sha256_file},
};

/// Evaluates snapshot freshness and semantic model availability against the live corpus
/// and model cache. File hashes are cached by (size, mtime), so repeated evaluations only
/// re-hash files whose metadata changed.
pub struct StatusMonitor {
    config: AppConfig,
    hashes: Mutex<FileHashCache>,
}

impl StatusMonitor {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            config: config.clone(),
            hashes: Mutex::new(FileHashCache::default()),
        }
    }

    /// Status of the snapshot CURRENT points at.
    pub fn current_status(&self) -> SnapshotStatus {
        let current_path = self.config.index_dir.join(CURRENT_FILE);
        if !current_path.is_file() {
            return SnapshotStatus::unusable(
                SnapshotState::Missing,
                format!("{} is missing", current_path.display()),
            );
        }
        match Snapshot::open(&self.config.index_dir, &self.config.snapshot_cache_dir) {
            Ok(snapshot) => self.evaluate(&snapshot.manifest),
            Err(error) => SnapshotStatus::unusable(SnapshotState::Invalid, error.to_string()),
        }
    }

    /// Status of the snapshot described by `manifest`.
    pub fn evaluate(&self, manifest: &SnapshotManifest) -> SnapshotStatus {
        let mut hashes = self.hashes.lock();
        let (mut reasons, freshness) = self.freshness(&mut hashes, manifest);
        let semantic_degraded_reasons = self.semantic_degradation(&mut hashes, manifest);
        let state = if !reasons.is_empty() {
            SnapshotState::Stale
        } else if !semantic_degraded_reasons.is_empty() {
            SnapshotState::DegradedModel
        } else {
            SnapshotState::Ready
        };
        reasons.extend(semantic_degraded_reasons.iter().cloned());
        SnapshotStatus {
            state,
            snapshot_id: Some(manifest.snapshot_id.clone()),
            counts: Some(manifest.counts.clone()),
            reasons,
            freshness,
            semantic_degraded_reasons,
        }
    }

    /// Whether the corpus or the production model contract no longer match `manifest`.
    pub fn is_stale(&self, manifest: &SnapshotManifest) -> bool {
        let (reasons, _) = self.freshness(&mut self.hashes.lock(), manifest);
        !reasons.is_empty()
    }

    /// Reasons semantic retrieval is unusable for `manifest`; empty when it is usable.
    pub fn semantic_degraded_reasons(&self, manifest: &SnapshotManifest) -> Vec<String> {
        self.semantic_degradation(&mut self.hashes.lock(), manifest)
    }

    fn freshness(
        &self,
        hashes: &mut FileHashCache,
        manifest: &SnapshotManifest,
    ) -> (Vec<String>, Vec<ArchiveFreshness>) {
        let config = &self.config;
        let mut stale = Vec::new();
        let mut freshness = Vec::with_capacity(manifest.archives.len());
        match hashes.hash(&config.corpus_manifest, HashKind::Blake3) {
            Ok(hash) if hash != manifest.corpus_manifest_blake3 => {
                stale.push("corpus manifest hash changed".into())
            }
            Err(error) => stale.push(error.to_string()),
            _ => {}
        }
        let corpus = match CorpusManifest::load(&config.corpus_manifest) {
            Ok(corpus) => Some(corpus),
            Err(error) => {
                stale.push(error.to_string());
                None
            }
        };
        if let Some(corpus) = &corpus
            && corpus.archives.len() != manifest.archives.len()
        {
            stale.push("corpus archive count changed".into());
        }
        for expected in &manifest.archives {
            let current = corpus.as_ref().and_then(|corpus| {
                corpus
                    .archives
                    .iter()
                    .find(|archive| archive.id == expected.id)
            });
            let metadata_matches = current.is_some_and(|archive| {
                archive.sha256.eq_ignore_ascii_case(&expected.sha256)
                    && archive.path == expected.path
                    && archive.entry_count == expected.entry_count
                    && archive.uncompressed_bytes == expected.uncompressed_bytes
            });
            let path = current.map_or(expected.path.as_str(), |archive| archive.path.as_str());
            let mut archive_reasons = Vec::new();
            if !metadata_matches {
                archive_reasons.push(format!("archive {} metadata changed", expected.id));
            }
            let actual_sha256 = match hashes.hash(&config.corpus_dir.join(path), HashKind::Sha256) {
                Ok(hash) => {
                    if !hash.eq_ignore_ascii_case(&expected.sha256) {
                        archive_reasons.push(format!("archive {} content changed", expected.id));
                    }
                    Some(hash)
                }
                Err(error) => {
                    archive_reasons.push(error.to_string());
                    None
                }
            };
            let fresh = metadata_matches
                && actual_sha256
                    .as_deref()
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&expected.sha256));
            stale.extend(archive_reasons.iter().cloned());
            freshness.push(ArchiveFreshness {
                archive_id: expected.id.clone(),
                path: path.to_owned(),
                expected_sha256: Some(expected.sha256.clone()),
                actual_sha256,
                metadata_matches,
                fresh,
                reason: (!archive_reasons.is_empty()).then(|| archive_reasons.join("; ")),
            });
        }
        if let Some(corpus) = &corpus {
            for archive in corpus.archives.iter().filter(|archive| {
                !manifest
                    .archives
                    .iter()
                    .any(|expected| expected.id == archive.id)
            }) {
                let reason = format!("archive {} is absent from the snapshot", archive.id);
                stale.push(reason.clone());
                freshness.push(ArchiveFreshness {
                    archive_id: archive.id.clone(),
                    path: archive.path.clone(),
                    expected_sha256: None,
                    actual_sha256: hashes
                        .hash(&config.corpus_dir.join(&archive.path), HashKind::Sha256)
                        .ok(),
                    metadata_matches: false,
                    fresh: false,
                    reason: Some(reason),
                });
            }
        }
        if let Ok(spec) = FastEmbedder::production_spec()
            && spec != manifest.model
        {
            stale.push("production embedding model contract changed".into());
        }
        (stale, freshness)
    }

    fn semantic_degradation(
        &self,
        hashes: &mut FileHashCache,
        manifest: &SnapshotManifest,
    ) -> Vec<String> {
        let mut degraded = Vec::new();
        if let Err(error) = FastEmbedder::production_spec() {
            degraded.push(error.to_string());
        }
        for artifact in model_snapshot_artifacts(&manifest.model_artifacts) {
            match ensure_relative_artifact(&artifact.path) {
                Ok(relative) => match hashes.hash(
                    &self.config.model_cache_dir.join(relative),
                    HashKind::Blake3,
                ) {
                    Ok(hash) if hash != artifact.blake3 => {
                        degraded.push(format!("model artifact {} changed", artifact.path))
                    }
                    Err(error) => degraded.push(error.to_string()),
                    _ => {}
                },
                Err(error) => degraded.push(error.to_string()),
            }
        }
        degraded
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum HashKind {
    Blake3,
    Sha256,
}

struct CachedHash {
    len: u64,
    modified: SystemTime,
    hash: String,
}

#[derive(Default)]
struct FileHashCache {
    entries: HashMap<(PathBuf, HashKind), CachedHash>,
}

impl FileHashCache {
    fn hash(&mut self, path: &Path, kind: HashKind) -> Result<String, SnapshotError> {
        let metadata = fs::metadata(path).map_err(|source| SnapshotError::io(path, source))?;
        let modified = metadata
            .modified()
            .map_err(|source| SnapshotError::io(path, source))?;
        let key = (path.to_path_buf(), kind);
        if let Some(cached) = self.entries.get(&key)
            && cached.len == metadata.len()
            && cached.modified == modified
        {
            return Ok(cached.hash.clone());
        }
        let hash = match kind {
            HashKind::Blake3 => blake3_file(path)?,
            HashKind::Sha256 => sha256_file(path)?,
        };
        self.entries.insert(
            key,
            CachedHash {
                len: metadata.len(),
                modified,
                hash: hash.clone(),
            },
        );
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        time::{Duration, SystemTime},
    };

    use super::{FileHashCache, HashKind};

    #[test]
    fn file_hash_cache_rehashes_only_on_metadata_change() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("archive.zip");
        fs::write(&path, b"first").unwrap();
        let pinned = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(pinned)
            .unwrap();
        let mut cache = FileHashCache::default();
        let first = cache.hash(&path, HashKind::Sha256).unwrap();

        fs::write(&path, b"other").unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(pinned).unwrap();
        assert_eq!(cache.hash(&path, HashKind::Sha256).unwrap(), first);

        file.set_modified(pinned + Duration::from_secs(1)).unwrap();
        let changed = cache.hash(&path, HashKind::Sha256).unwrap();
        assert_ne!(changed, first);
        assert_ne!(
            cache.hash(&path, HashKind::Blake3).unwrap(),
            cache.hash(&path, HashKind::Sha256).unwrap()
        );
    }
}
