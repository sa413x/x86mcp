mod artifacts;
mod builder;
mod manifest;
mod opener;
mod publisher;

use std::{io, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    catalog::{CatalogCounts, CatalogReader},
    index::{IndexError, LexicalSearcher, VectorError, VectorReader},
};

pub use builder::SnapshotBuilder;
pub use manifest::{
    ArchiveFingerprint, ComponentChecksums, CompressedArtifact, CompressedPart, EntryFingerprint,
    SNAPSHOT_SCHEMA_VERSION, SnapshotManifest,
};
pub use opener::snapshot_status;
pub use publisher::SnapshotPublisher;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("snapshot JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("corpus error: {0}")]
    Corpus(#[from] crate::corpus::CorpusError),
    #[error("ingestion error: {0}")]
    Ingest(#[from] crate::ingest::IngestError),
    #[error("catalog error: {0}")]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error("lexical index error: {0}")]
    Index(#[from] IndexError),
    #[error("vector index error: {0}")]
    Vector(#[from] VectorError),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("invalid snapshot: {0}")]
    Invalid(String),
}

impl SnapshotError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub struct Snapshot {
    pub manifest: SnapshotManifest,
    pub catalog: CatalogReader,
    pub lexical: LexicalSearcher,
    pub vectors: VectorReader,
    pub generation_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct BuildReport {
    pub snapshot_id: String,
    pub built: bool,
    pub reused_embeddings: u64,
    pub reused_parsed_documents: u64,
    pub embedded_embeddings: u64,
    pub counts: CatalogCounts,
    pub warnings: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotState {
    Ready,
    Stale,
    DegradedModel,
    Missing,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ArchiveFreshness {
    pub archive_id: String,
    pub path: String,
    pub expected_sha256: Option<String>,
    pub actual_sha256: Option<String>,
    pub metadata_matches: bool,
    pub fresh: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SnapshotStatus {
    pub state: SnapshotState,
    pub snapshot_id: Option<String>,
    pub counts: Option<CatalogCounts>,
    pub reasons: Vec<String>,
    pub freshness: Vec<ArchiveFreshness>,
}

impl SnapshotStatus {
    pub fn is_usable(&self) -> bool {
        matches!(
            self.state,
            SnapshotState::Ready | SnapshotState::Stale | SnapshotState::DegradedModel
        )
    }

    pub fn is_success(&self) -> bool {
        matches!(
            self.state,
            SnapshotState::Ready | SnapshotState::DegradedModel
        )
    }
}
