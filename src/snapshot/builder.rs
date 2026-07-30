use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result as AnyResult;
use rayon::prelude::*;
use serde::Serialize;

use crate::{
    catalog::{CatalogReader, CatalogWriter, schema::CATALOG_SCHEMA_VERSION},
    config::AppConfig,
    corpus::{CorpusManifest, CorpusReader},
    domain::{chunk::SearchChunk, document::ArchiveDocument},
    index::{
        Embedder, LEXICAL_SCHEMA_VERSION, LexicalWriter, ModelArtifact, ModelSpec,
        VECTOR_FORMAT_VERSION, VectorWriter,
    },
    ingest::{
        ChunkConfig, INGEST_SCHEMA_VERSION, ParsedDocument, TokenCounter, chunk_document,
        parse_document, resolve_corpus_references,
    },
};

use super::{
    ArchiveFingerprint, BuildReport, ComponentChecksums, EntryFingerprint, Snapshot, SnapshotError,
    SnapshotManifest, SnapshotPublisher,
    artifacts::compress_artifact,
    manifest::{
        CATALOG_FILE, CATALOG_ZSTD_FILE, LEXICAL_DIR, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION,
        VECTOR_FILE, VECTOR_ZSTD_FILE, blake3_file,
    },
};

const EMBEDDING_BATCH_SIZE: usize = 64;
const MAX_GIT_BLOB_BYTES: u64 = 99 * 1024 * 1024;
const PART_RAW_BYTES: u64 = 64 * 1024 * 1024;

pub struct SnapshotBuilder<'a> {
    config: &'a AppConfig,
    embedder: &'a dyn Embedder,
}

impl<'a> SnapshotBuilder<'a> {
    pub fn new(config: &'a AppConfig, embedder: &'a dyn Embedder) -> Self {
        Self { config, embedder }
    }

    pub fn build(&self, force: bool) -> Result<BuildReport, SnapshotError> {
        let corpus_manifest = CorpusManifest::load(&self.config.corpus_manifest)?;
        let corpus_manifest_blake3 = blake3_file(&self.config.corpus_manifest)?;
        let documents = CorpusReader::new(corpus_manifest.clone(), self.config.corpus_dir.clone())
            .read_all()?;
        let archives = archive_fingerprints(&corpus_manifest);
        let entries = entry_fingerprints(&documents);
        let mut model_artifacts = self
            .embedder
            .artifact_hashes()
            .map_err(|error| SnapshotError::Embedding(error.to_string()))?;
        model_artifacts.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        let chunk_config = ChunkConfig::default();
        let snapshot_id = snapshot_id(
            &corpus_manifest_blake3,
            &archives,
            &entries,
            self.embedder.spec(),
            &model_artifacts,
            chunk_config,
        )?;

        if !force
            && let Ok(current) =
                Snapshot::open(&self.config.index_dir, &self.config.snapshot_cache_dir)
            && current.manifest.snapshot_id == snapshot_id
        {
            return Ok(BuildReport {
                snapshot_id,
                built: false,
                reused_embeddings: current.vectors.count(),
                reused_parsed_documents: current.manifest.counts.documents,
                embedded_embeddings: 0,
                warnings: current.manifest.counts.warnings,
                counts: current.manifest.counts.clone(),
            });
        }

        let current = Snapshot::open(&self.config.index_dir, &self.config.snapshot_cache_dir).ok();
        let vector_source = current
            .as_ref()
            .filter(|snapshot| snapshot.manifest.model == *self.embedder.spec());
        let current_catalog = current
            .as_ref()
            .filter(|snapshot| snapshot.manifest.ingest_schema_version == INGEST_SCHEMA_VERSION)
            .map(|snapshot| snapshot.catalog.clone());
        let counter = EmbedderCounter(self.embedder);
        let parsed_with_reuse = documents
            .par_iter()
            .map(
                |document| -> Result<(ParsedDocument, bool), SnapshotError> {
                    if let Some(catalog) = &current_catalog
                        && let Some(previous) = catalog.document(&document.meta.document_id)?
                        && previous.meta.content_sha256 == document.meta.content_sha256
                        && let Some(parsed) = catalog.parsed_document(&document.meta.document_id)?
                    {
                        return Ok((parsed, true));
                    }
                    Ok((parse_document(document)?, false))
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let reused_parsed_documents = parsed_with_reuse
            .iter()
            .filter(|(_, reused)| *reused)
            .count() as u64;
        let mut parsed = parsed_with_reuse
            .into_iter()
            .map(|(parsed, _)| parsed)
            .collect::<Vec<_>>();
        resolve_corpus_references(&mut parsed);
        let built = documents
            .into_par_iter()
            .zip(parsed)
            .map(|(document, parsed)| {
                let chunks = chunk_document(&document, &parsed, &counter, chunk_config)?;
                Ok(BuildDocument {
                    document,
                    parsed,
                    chunks,
                })
            })
            .collect::<AnyResult<Vec<_>>>()
            .map_err(|error| SnapshotError::Embedding(error.to_string()))?;

        fs::create_dir_all(&self.config.index_dir)
            .map_err(|source| SnapshotError::io(&self.config.index_dir, source))?;
        let candidate_dir = self.config.index_dir.join(format!(".build-{snapshot_id}"));
        if candidate_dir.exists() {
            fs::remove_dir_all(&candidate_dir)
                .map_err(|source| SnapshotError::io(&candidate_dir, source))?;
        }
        fs::create_dir_all(&candidate_dir)
            .map_err(|source| SnapshotError::io(&candidate_dir, source))?;
        let mut guard = CandidateGuard::new(candidate_dir.clone());

        let catalog_path = candidate_dir.join(CATALOG_FILE);
        let mut catalog_writer = CatalogWriter::create(&catalog_path)?;
        let mut vector_row = 0_u64;
        for document in &built {
            vector_row = catalog_writer.write_document(
                &document.document,
                &document.parsed,
                &document.chunks,
                vector_row,
            )?;
        }
        catalog_writer.finish()?;

        let all_chunks = built
            .into_iter()
            .flat_map(|document| document.chunks)
            .collect::<Vec<_>>();
        if vector_row != all_chunks.len() as u64 {
            return Err(SnapshotError::Invalid(format!(
                "catalog vector rows {vector_row} differ from chunk count {}",
                all_chunks.len()
            )));
        }
        let lexical_stats =
            LexicalWriter::create(&candidate_dir.join(LEXICAL_DIR))?.write(&all_chunks)?;
        let chunk_refs = all_chunks.iter().collect::<Vec<_>>();
        let (vectors, reused_embeddings, embedded_embeddings) =
            self.build_vectors(&chunk_refs, vector_source)?;
        let vector_path = candidate_dir.join(VECTOR_FILE);
        let vector_stats =
            VectorWriter::write(&vector_path, self.embedder.spec().dimension, &vectors)?;
        let catalog = CatalogReader::open(&catalog_path)?;
        catalog.integrity_check()?;
        let counts = catalog.counts()?;
        if counts.chunks != lexical_stats.document_count
            || counts.vectors != vector_stats.count
            || counts.documents != entries.len() as u64
        {
            return Err(SnapshotError::Invalid(
                "component counts differ before publication".into(),
            ));
        }
        let catalog_artifact = compress_artifact(
            &catalog_path,
            &candidate_dir,
            CATALOG_ZSTD_FILE,
            MAX_GIT_BLOB_BYTES,
            PART_RAW_BYTES,
        )?;
        let vector_artifact = compress_artifact(
            &vector_path,
            &candidate_dir,
            VECTOR_ZSTD_FILE,
            MAX_GIT_BLOB_BYTES,
            PART_RAW_BYTES,
        )?;
        let manifest = SnapshotManifest {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            snapshot_id: snapshot_id.clone(),
            created_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| SnapshotError::Invalid(error.to_string()))?
                .as_secs(),
            build_version: env!("CARGO_PKG_VERSION").into(),
            ingest_schema_version: INGEST_SCHEMA_VERSION,
            corpus_manifest_blake3,
            archives,
            entries,
            catalog_schema_version: CATALOG_SCHEMA_VERSION,
            lexical_schema_version: LEXICAL_SCHEMA_VERSION,
            vector_format_version: VECTOR_FORMAT_VERSION,
            vector_count: vector_stats.count,
            vector_dimension: vector_stats.dimension,
            vector_payload_blake3: vector_stats.payload_hash,
            model: self.embedder.spec().clone(),
            model_artifacts,
            counts: counts.clone(),
            components: ComponentChecksums {
                catalog: catalog_artifact,
                lexical_blake3: lexical_stats.component_checksum,
                vectors: vector_artifact,
            },
        };
        manifest.write(&candidate_dir.join(SNAPSHOT_FILE))?;
        drop(catalog);
        fs::remove_file(&catalog_path)
            .map_err(|source| SnapshotError::io(&catalog_path, source))?;
        fs::remove_file(&vector_path).map_err(|source| SnapshotError::io(&vector_path, source))?;
        drop(current);
        SnapshotPublisher::publish(
            &self.config.index_dir,
            &candidate_dir,
            &snapshot_id,
            &self.config.snapshot_cache_dir,
        )?;
        guard.disarm();

        Ok(BuildReport {
            snapshot_id,
            built: true,
            reused_embeddings,
            reused_parsed_documents,
            embedded_embeddings,
            warnings: counts.warnings,
            counts,
        })
    }

    fn build_vectors(
        &self,
        chunks: &[&SearchChunk],
        current: Option<&Snapshot>,
    ) -> Result<(Vec<Vec<f32>>, u64, u64), SnapshotError> {
        let mut reuse = HashMap::new();
        if let Some(snapshot) = current {
            for record in snapshot.catalog.vector_reuse_records()? {
                reuse
                    .entry(record.content_hash)
                    .or_insert(record.vector_row);
            }
        }
        let mut vectors = vec![None; chunks.len()];
        let mut missing = BTreeMap::<String, Vec<usize>>::new();
        let mut reused = 0_u64;
        for (index, chunk) in chunks.iter().enumerate() {
            if let (Some(snapshot), Some(&row)) = (current, reuse.get(&chunk.content_hash)) {
                vectors[index] = Some(snapshot.vectors.row(row)?.to_vec());
                reused += 1;
            } else {
                missing
                    .entry(chunk.content_hash.clone())
                    .or_default()
                    .push(index);
            }
        }

        let groups = missing.into_iter().collect::<Vec<_>>();
        for batch in groups.chunks(EMBEDDING_BATCH_SIZE) {
            let texts = batch
                .iter()
                .map(|(_, indices)| chunks[indices[0]].text.clone())
                .collect::<Vec<_>>();
            let embedded = self
                .embedder
                .embed_passages(&texts)
                .map_err(|error| SnapshotError::Embedding(error.to_string()))?;
            if embedded.len() != batch.len() {
                return Err(SnapshotError::Invalid(format!(
                    "embedder returned {} vectors for {} passages",
                    embedded.len(),
                    batch.len()
                )));
            }
            for ((_, indices), vector) in batch.iter().zip(embedded) {
                for &index in indices {
                    vectors[index] = Some(vector.clone());
                }
            }
        }
        let embedded = chunks.len() as u64 - reused;
        let vectors = vectors
            .into_iter()
            .enumerate()
            .map(|(row, vector)| {
                vector.ok_or_else(|| {
                    SnapshotError::Invalid(format!("missing embedding for vector row {row}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((vectors, reused, embedded))
    }
}

struct BuildDocument {
    document: ArchiveDocument,
    parsed: ParsedDocument,
    chunks: Vec<SearchChunk>,
}

struct EmbedderCounter<'a>(&'a dyn Embedder);

impl TokenCounter for EmbedderCounter<'_> {
    fn count(&self, text: &str) -> AnyResult<usize> {
        self.0.count_tokens(text)
    }
}

#[derive(Serialize)]
struct BuildIdentity<'a> {
    snapshot_schema_version: u32,
    build_version: &'static str,
    ingest_schema_version: u32,
    corpus_manifest_blake3: &'a str,
    archives: &'a [ArchiveFingerprint],
    entries: &'a [EntryFingerprint],
    catalog_schema_version: u32,
    lexical_schema_version: u32,
    vector_format_version: u32,
    model: &'a ModelSpec,
    model_artifacts: &'a [ModelArtifact],
    target_tokens: usize,
    overlap_tokens: usize,
    table_rows_per_chunk: usize,
}

fn snapshot_id(
    corpus_manifest_blake3: &str,
    archives: &[ArchiveFingerprint],
    entries: &[EntryFingerprint],
    model: &ModelSpec,
    model_artifacts: &[ModelArtifact],
    chunk_config: ChunkConfig,
) -> Result<String, SnapshotError> {
    let identity = BuildIdentity {
        snapshot_schema_version: SNAPSHOT_SCHEMA_VERSION,
        build_version: env!("CARGO_PKG_VERSION"),
        ingest_schema_version: INGEST_SCHEMA_VERSION,
        corpus_manifest_blake3,
        archives,
        entries,
        catalog_schema_version: CATALOG_SCHEMA_VERSION,
        lexical_schema_version: LEXICAL_SCHEMA_VERSION,
        vector_format_version: VECTOR_FORMAT_VERSION,
        model,
        model_artifacts,
        target_tokens: chunk_config.target_tokens,
        overlap_tokens: chunk_config.overlap_tokens,
        table_rows_per_chunk: chunk_config.table_rows_per_chunk,
    };
    let canonical = serde_json::to_vec(&identity)?;
    Ok(blake3::hash(&canonical).to_hex().to_string())
}

fn archive_fingerprints(manifest: &CorpusManifest) -> Vec<ArchiveFingerprint> {
    let mut archives = manifest
        .archives
        .iter()
        .map(|archive| ArchiveFingerprint {
            id: archive.id.clone(),
            vendor: archive.vendor,
            path: archive.path.clone(),
            sha256: archive.sha256.to_ascii_lowercase(),
            entry_count: archive.entry_count,
            uncompressed_bytes: archive.uncompressed_bytes,
        })
        .collect::<Vec<_>>();
    archives.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    archives
}

fn entry_fingerprints(documents: &[ArchiveDocument]) -> Vec<EntryFingerprint> {
    let mut entries = documents
        .iter()
        .map(|document| EntryFingerprint {
            document_id: document.meta.document_id.clone(),
            archive_id: document.meta.archive_id.clone(),
            entry_path: document.meta.entry_path.clone(),
            sha256: document.meta.content_sha256.clone(),
            byte_len: document.meta.byte_len,
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.document_id.cmp(&right.document_id));
    entries
}

struct CandidateGuard {
    path: PathBuf,
    armed: bool,
}

impl CandidateGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CandidateGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
