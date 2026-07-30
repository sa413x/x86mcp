use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    catalog::CatalogCounts,
    domain::Vendor,
    index::{ModelArtifact, ModelSpec},
};

use super::SnapshotError;

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 2;
pub(crate) const SNAPSHOT_FILE: &str = "snapshot.json";
pub(crate) const CATALOG_FILE: &str = "catalog.sqlite3";
pub(crate) const LEXICAL_DIR: &str = "lexical";
pub(crate) const VECTOR_FILE: &str = "vectors.f32";
pub(crate) const CATALOG_ZSTD_FILE: &str = "catalog.sqlite3.zst";
pub(crate) const VECTOR_ZSTD_FILE: &str = "vectors.f32.zst";
pub(crate) const CURRENT_FILE: &str = "CURRENT";
pub(crate) const SNAPSHOTS_DIR: &str = "snapshots";

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ArchiveFingerprint {
    pub id: String,
    pub vendor: Vendor,
    pub path: String,
    pub sha256: String,
    pub entry_count: u32,
    pub uncompressed_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct EntryFingerprint {
    pub document_id: String,
    pub archive_id: String,
    pub entry_path: String,
    pub sha256: String,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CompressedPart {
    pub path: String,
    pub compressed_bytes: u64,
    pub compressed_blake3: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CompressedArtifact {
    pub raw_bytes: u64,
    pub raw_blake3: String,
    pub parts: Vec<CompressedPart>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ComponentChecksums {
    pub catalog: CompressedArtifact,
    pub lexical_blake3: String,
    pub vectors: CompressedArtifact,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub created_unix_seconds: u64,
    pub build_version: String,
    #[serde(default)]
    pub ingest_schema_version: u32,
    pub corpus_manifest_blake3: String,
    pub archives: Vec<ArchiveFingerprint>,
    pub entries: Vec<EntryFingerprint>,
    pub catalog_schema_version: u32,
    pub lexical_schema_version: u32,
    pub vector_format_version: u32,
    pub vector_count: u64,
    pub vector_dimension: usize,
    pub vector_payload_blake3: String,
    pub model: ModelSpec,
    pub model_artifacts: Vec<ModelArtifact>,
    pub counts: CatalogCounts,
    pub components: ComponentChecksums,
}

impl SnapshotManifest {
    pub fn load(path: &Path) -> Result<Self, SnapshotError> {
        let file = File::open(path).map_err(|source| SnapshotError::io(path, source))?;
        serde_json::from_reader(BufReader::new(file)).map_err(SnapshotError::from)
    }

    pub fn write(&self, path: &Path) -> Result<(), SnapshotError> {
        let mut file =
            AtomicWriteFile::open(path).map_err(|source| SnapshotError::io(path, source))?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")
            .map_err(|source| SnapshotError::io(path, source))?;
        file.commit()
            .map_err(|source| SnapshotError::io(path, source))
    }
}

pub(crate) fn blake3_file(path: &Path) -> Result<String, SnapshotError> {
    let file = File::open(path).map_err(|source| SnapshotError::io(path, source))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| SnapshotError::io(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, SnapshotError> {
    let file = File::open(path).map_err(|source| SnapshotError::io(path, source))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| SnapshotError::io(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn ensure_relative_artifact(path: &str) -> Result<PathBuf, SnapshotError> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.ends_with(':'))
    {
        return Err(SnapshotError::Invalid(format!(
            "unsafe model artifact path {path:?}"
        )));
    }
    Ok(PathBuf::from(normalized))
}
