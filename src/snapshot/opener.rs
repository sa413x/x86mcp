use std::{fs, path::Path};

use crate::{
    catalog::schema::CATALOG_SCHEMA_VERSION,
    config::AppConfig,
    corpus::CorpusManifest,
    index::{
        FastEmbedder, LEXICAL_SCHEMA_VERSION, LexicalSearcher, VECTOR_FORMAT_VERSION, VectorReader,
        lexical_component_checksum, model_snapshot_artifacts,
    },
};

use super::{
    ArchiveFreshness, Snapshot, SnapshotError, SnapshotState, SnapshotStatus,
    artifacts::materialize_artifact,
    manifest::{
        CATALOG_FILE, CURRENT_FILE, LEXICAL_DIR, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION,
        SNAPSHOTS_DIR, VECTOR_FILE, blake3_file, ensure_relative_artifact, sha256_file,
    },
};

impl Snapshot {
    pub fn open(index_dir: &Path, snapshot_cache_dir: &Path) -> Result<Self, SnapshotError> {
        let current_path = index_dir.join(CURRENT_FILE);
        let snapshot_id = fs::read_to_string(&current_path)
            .map_err(|source| SnapshotError::io(&current_path, source))?;
        let snapshot_id = snapshot_id.trim();
        validate_snapshot_id(snapshot_id)?;
        let generation_path = index_dir.join(SNAPSHOTS_DIR).join(snapshot_id);
        Self::open_generation(&generation_path, snapshot_id, snapshot_cache_dir)
    }

    pub(crate) fn open_generation(
        generation_path: &Path,
        expected_id: &str,
        snapshot_cache_dir: &Path,
    ) -> Result<Self, SnapshotError> {
        validate_snapshot_id(expected_id)?;
        let manifest = super::SnapshotManifest::load(&generation_path.join(SNAPSHOT_FILE))?;
        validate_manifest_contract(&manifest, expected_id)?;

        let cache_generation = snapshot_cache_dir.join(expected_id);
        let catalog_path = materialize_artifact(
            generation_path,
            &cache_generation.join(CATALOG_FILE),
            &manifest.components.catalog,
        )?;
        let lexical_path = generation_path.join(LEXICAL_DIR);
        let vector_path = materialize_artifact(
            generation_path,
            &cache_generation.join(VECTOR_FILE),
            &manifest.components.vectors,
        )?;
        ensure_checksum(
            "lexical",
            &manifest.components.lexical_blake3,
            &lexical_component_checksum(&lexical_path)?,
        )?;

        let catalog = crate::catalog::CatalogReader::open(&catalog_path)?;
        catalog.integrity_check()?;
        let counts = catalog.counts()?;
        if counts != manifest.counts {
            return Err(SnapshotError::Invalid(format!(
                "catalog counts differ from snapshot manifest: {:?} != {:?}",
                counts, manifest.counts
            )));
        }
        let lexical = LexicalSearcher::open(&lexical_path)?;
        if lexical.document_count() != counts.chunks {
            return Err(SnapshotError::Invalid(format!(
                "lexical document count {} differs from chunk count {}",
                lexical.document_count(),
                counts.chunks
            )));
        }
        let vectors = VectorReader::open(&vector_path, manifest.vector_dimension)?;
        if vectors.count() != manifest.vector_count || vectors.count() != counts.vectors {
            return Err(SnapshotError::Invalid(format!(
                "vector count {} differs from manifest/catalog counts {}/{}",
                vectors.count(),
                manifest.vector_count,
                counts.vectors
            )));
        }
        ensure_checksum(
            "vector payload",
            &manifest.vector_payload_blake3,
            vectors.payload_hash(),
        )?;
        if manifest.archives.len() as u64 != counts.archives
            || manifest.entries.len() as u64 != counts.documents
        {
            return Err(SnapshotError::Invalid(
                "archive or entry fingerprint count differs from catalog".into(),
            ));
        }

        Ok(Self {
            manifest,
            catalog,
            lexical,
            vectors,
            generation_path: generation_path.to_path_buf(),
        })
    }
}

pub fn snapshot_status(config: &AppConfig) -> SnapshotStatus {
    let current_path = config.index_dir.join(CURRENT_FILE);
    if !current_path.is_file() {
        return SnapshotStatus {
            state: SnapshotState::Missing,
            snapshot_id: None,
            counts: None,
            reasons: vec![format!("{} is missing", current_path.display())],
            freshness: Vec::new(),
        };
    }
    let snapshot = match Snapshot::open(&config.index_dir, &config.snapshot_cache_dir) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return SnapshotStatus {
                state: SnapshotState::Invalid,
                snapshot_id: None,
                counts: None,
                reasons: vec![error.to_string()],
                freshness: Vec::new(),
            };
        }
    };

    let mut stale = Vec::new();
    let mut degraded = Vec::new();
    let mut freshness = Vec::with_capacity(snapshot.manifest.archives.len());
    match blake3_file(&config.corpus_manifest) {
        Ok(hash) if hash != snapshot.manifest.corpus_manifest_blake3 => {
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
        && corpus.archives.len() != snapshot.manifest.archives.len()
    {
        stale.push("corpus archive count changed".into());
    }
    for expected in &snapshot.manifest.archives {
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
        let actual_sha256 = match sha256_file(&config.corpus_dir.join(path)) {
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
            !snapshot
                .manifest
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
                actual_sha256: sha256_file(&config.corpus_dir.join(&archive.path)).ok(),
                metadata_matches: false,
                fresh: false,
                reason: Some(reason),
            });
        }
    }

    match FastEmbedder::production_spec() {
        Ok(spec) if spec != snapshot.manifest.model => {
            stale.push("production embedding model contract changed".into())
        }
        Err(error) => degraded.push(error.to_string()),
        _ => {}
    }
    for artifact in model_snapshot_artifacts(&snapshot.manifest.model_artifacts) {
        match ensure_relative_artifact(&artifact.path) {
            Ok(relative) => match blake3_file(&config.model_cache_dir.join(relative)) {
                Ok(hash) if hash != artifact.blake3 => {
                    degraded.push(format!("model artifact {} changed", artifact.path))
                }
                Err(error) => degraded.push(error.to_string()),
                _ => {}
            },
            Err(error) => degraded.push(error.to_string()),
        }
    }

    let state = if !stale.is_empty() {
        SnapshotState::Stale
    } else if !degraded.is_empty() {
        SnapshotState::DegradedModel
    } else {
        SnapshotState::Ready
    };
    stale.extend(degraded);
    SnapshotStatus {
        state,
        snapshot_id: Some(snapshot.manifest.snapshot_id.clone()),
        counts: Some(snapshot.manifest.counts.clone()),
        reasons: stale,
        freshness,
    }
}

fn validate_manifest_contract(
    manifest: &super::SnapshotManifest,
    expected_id: &str,
) -> Result<(), SnapshotError> {
    if manifest.schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(SnapshotError::Invalid(format!(
            "unsupported snapshot schema {}",
            manifest.schema_version
        )));
    }
    if manifest.snapshot_id != expected_id {
        return Err(SnapshotError::Invalid(format!(
            "snapshot id mismatch: expected {expected_id}, got {}",
            manifest.snapshot_id
        )));
    }
    if manifest.catalog_schema_version != CATALOG_SCHEMA_VERSION
        || manifest.lexical_schema_version != LEXICAL_SCHEMA_VERSION
        || manifest.vector_format_version != VECTOR_FORMAT_VERSION
    {
        return Err(SnapshotError::Invalid(
            "component schema version mismatch".into(),
        ));
    }
    if manifest.vector_dimension != manifest.model.dimension {
        return Err(SnapshotError::Invalid(
            "vector dimension differs from model contract".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_snapshot_id(snapshot_id: &str) -> Result<(), SnapshotError> {
    if snapshot_id.len() != 64
        || !snapshot_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SnapshotError::Invalid(format!(
            "unsafe snapshot id {snapshot_id:?}"
        )));
    }
    Ok(())
}

fn ensure_checksum(component: &str, expected: &str, actual: &str) -> Result<(), SnapshotError> {
    if expected != actual {
        return Err(SnapshotError::Invalid(format!(
            "{component} checksum mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}
