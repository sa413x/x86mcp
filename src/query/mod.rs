mod context;
mod exact;
mod hybrid;
mod planner;
mod ranking;
mod types;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use crate::{
    catalog::VectorChunkMetadata,
    domain::{block::SourceBlock, chunk::SearchChunk, citation::Citation},
    index::{Embedder, FastEmbedder, ModelSpec},
    ingest::SectionNode,
    snapshot::{Snapshot, SnapshotState, SnapshotStatus},
};

pub use types::*;

pub type EmbedderFactory =
    Arc<dyn Fn() -> anyhow::Result<Arc<dyn Embedder>> + Send + Sync + 'static>;

pub struct QueryEngine {
    pub(crate) snapshot: Snapshot,
    pub(crate) status: SnapshotStatus,
    pub(crate) vector_metadata: Vec<VectorChunkMetadata>,
    metadata_by_chunk: HashMap<String, usize>,
    semantic: SemanticRuntime,
}

impl QueryEngine {
    pub fn new(
        snapshot: Snapshot,
        status: SnapshotStatus,
        semantic_factory: Option<EmbedderFactory>,
    ) -> Result<Self, QueryError> {
        if status
            .snapshot_id
            .as_deref()
            .is_some_and(|id| id != snapshot.manifest.snapshot_id)
        {
            return Err(QueryError::InvalidInput(
                "status and opened snapshot IDs differ".into(),
            ));
        }
        let vector_metadata = snapshot.catalog.vector_metadata()?;
        if vector_metadata.len() as u64 != snapshot.vectors.count()
            || vector_metadata
                .iter()
                .enumerate()
                .any(|(index, metadata)| metadata.vector_row != index as u64)
        {
            return Err(QueryError::InvalidInput(
                "catalog vector metadata is not contiguous".into(),
            ));
        }
        let metadata_by_chunk = vector_metadata
            .iter()
            .enumerate()
            .map(|(index, metadata)| (metadata.chunk_id.clone(), index))
            .collect();
        Ok(Self {
            snapshot,
            status,
            vector_metadata,
            metadata_by_chunk,
            semantic: SemanticRuntime::new(semantic_factory),
        })
    }

    pub fn production(
        snapshot: Snapshot,
        status: SnapshotStatus,
        model_cache_dir: PathBuf,
    ) -> Result<Self, QueryError> {
        let factory: EmbedderFactory = Arc::new(move || {
            Ok(Arc::new(FastEmbedder::new(model_cache_dir.clone())?) as Arc<dyn Embedder>)
        });
        Self::new(snapshot, status, Some(factory))
    }

    pub fn index_status(&self) -> IndexStatusResponse {
        IndexStatusResponse {
            status: self.status.clone(),
            counts: self.snapshot.manifest.counts.clone(),
            manifest: self.snapshot.manifest.clone(),
        }
    }

    pub(crate) fn state(&self, semantic_degraded_reason: Option<String>) -> RetrievalState {
        RetrievalState {
            snapshot_id: self.snapshot.manifest.snapshot_id.clone(),
            stale: self.status.state == SnapshotState::Stale,
            semantic_degraded_reason,
        }
    }

    pub(crate) fn base_semantic_degraded_reason(&self) -> Option<String> {
        (self.status.state == SnapshotState::DegradedModel).then(|| {
            if self.status.reasons.is_empty() {
                "semantic model artifacts are unavailable".into()
            } else {
                self.status.reasons.join("; ")
            }
        })
    }

    pub(crate) fn semantic_embedder(&self) -> Result<Arc<dyn Embedder>, String> {
        if let Some(reason) = self.base_semantic_degraded_reason() {
            return Err(reason);
        }
        self.semantic.get(&self.snapshot.manifest.model)
    }

    pub(crate) fn metadata(&self, chunk_id: &str) -> Option<&VectorChunkMetadata> {
        self.metadata_by_chunk
            .get(chunk_id)
            .map(|index| &self.vector_metadata[*index])
    }

    pub(crate) fn citation_for_chunk(&self, chunk: &SearchChunk) -> Result<Citation, QueryError> {
        let document = self
            .snapshot
            .catalog
            .document(&chunk.document_id)?
            .ok_or_else(|| {
                QueryError::InvalidInput(format!(
                    "chunk {} references a missing document",
                    chunk.chunk_id
                ))
            })?;
        Ok(Citation {
            vendor: chunk.vendor,
            document_id: chunk.document_id.clone(),
            entry_path: document.meta.entry_path,
            section_id: chunk.section_id.clone(),
            heading_path: chunk.heading_path.clone(),
            span: chunk.span.clone(),
            source_url: None,
        })
    }

    pub(crate) fn citation_for_block(&self, block: &SourceBlock) -> Result<Citation, QueryError> {
        let document = self
            .snapshot
            .catalog
            .document(&block.document_id)?
            .ok_or_else(|| {
                QueryError::InvalidInput(format!(
                    "block {} references a missing document",
                    block.block_id
                ))
            })?;
        Ok(Citation {
            vendor: document.meta.vendor,
            document_id: block.document_id.clone(),
            entry_path: document.meta.entry_path,
            section_id: block.section_id.clone(),
            heading_path: block.heading_path.clone(),
            span: block.span.clone(),
            source_url: None,
        })
    }

    pub(crate) fn citation_for_section(
        &self,
        document_id: &str,
        section: &SectionNode,
    ) -> Result<Citation, QueryError> {
        let document = self
            .snapshot
            .catalog
            .document(document_id)?
            .ok_or_else(|| QueryError::InvalidInput("section document is missing".into()))?;
        Ok(Citation {
            vendor: document.meta.vendor,
            document_id: document_id.to_owned(),
            entry_path: document.meta.entry_path,
            section_id: section.section_id.clone(),
            heading_path: section.heading_path.clone(),
            span: section.span.clone(),
            source_url: None,
        })
    }

    pub(crate) fn snippet(text: &str) -> String {
        const MAX_CHARS: usize = 700;
        let mut output = text.chars().take(MAX_CHARS).collect::<String>();
        if text.chars().count() > MAX_CHARS {
            output.push('…');
        }
        output
    }
}

struct SemanticRuntime {
    factory: Option<EmbedderFactory>,
    initialized: OnceLock<Result<Arc<dyn Embedder>, String>>,
}

impl SemanticRuntime {
    fn new(factory: Option<EmbedderFactory>) -> Self {
        Self {
            factory,
            initialized: OnceLock::new(),
        }
    }

    fn get(&self, expected: &ModelSpec) -> Result<Arc<dyn Embedder>, String> {
        let result = self.initialized.get_or_init(|| {
            let factory = self
                .factory
                .as_ref()
                .ok_or_else(|| "semantic model is disabled".to_owned())?;
            let embedder = factory().map_err(|error| error.to_string())?;
            if embedder.spec() != expected {
                return Err(format!(
                    "semantic model contract mismatch: expected {}, got {}",
                    expected.code,
                    embedder.spec().code
                ));
            }
            Ok(embedder)
        });
        result.as_ref().map(Arc::clone).map_err(Clone::clone)
    }
}
