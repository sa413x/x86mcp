use std::{fs, path::Path};

use crate::{
    catalog::schema::CATALOG_SCHEMA_VERSION,
    config::AppConfig,
    index::{
        LEXICAL_SCHEMA_VERSION, LexicalSearcher, VECTOR_FORMAT_VERSION, VectorReader,
        lexical_component_checksum,
    },
};

use super::{
    Snapshot, SnapshotError, SnapshotStatus, StatusMonitor,
    artifacts::materialize_artifact,
    manifest::{
        CATALOG_FILE, CURRENT_FILE, LEXICAL_DIR, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION,
        SNAPSHOTS_DIR, VECTOR_FILE,
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
    StatusMonitor::new(config).current_status()
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
