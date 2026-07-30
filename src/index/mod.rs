mod embedder;
mod lexical_reader;
mod lexical_writer;
mod schema;
mod tokenizer;
mod vector_reader;
mod vector_writer;

use std::io;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{Vendor, chunk::ChunkKind};

pub use embedder::{Embedder, FastEmbedder, ModelArtifact, ModelSpec};
pub use lexical_reader::LexicalSearcher;
pub use lexical_writer::LexicalWriter;
pub use tokenizer::{normalize_symbol, symbol_terms};
pub use vector_reader::VectorReader;
pub use vector_writer::VectorWriter;

pub const LEXICAL_SCHEMA_VERSION: u32 = schema::LEXICAL_SCHEMA_VERSION;
pub const VECTOR_FORMAT_VERSION: u32 = vector_writer::FORMAT_VERSION;
pub(crate) use embedder::{model_artifacts_match, model_snapshot_artifacts};
pub(crate) use lexical_writer::component_checksum as lexical_component_checksum;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("invalid lexical request: {0}")]
    InvalidRequest(String),
    #[error("duplicate chunk id: {0}")]
    DuplicateChunkId(String),
    #[error("corrupt lexical index: {0}")]
    Corrupt(String),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LexicalSearchRequest {
    pub words: Vec<String>,
    pub exact_symbols: Vec<String>,
    pub vendor: Option<Vendor>,
    pub document_id: Option<String>,
    pub kind: Option<ChunkKind>,
    pub limit: u32,
}

impl LexicalSearchRequest {
    pub fn from_query(query: &str, limit: u32) -> Result<Self, IndexError> {
        if !(1..=100).contains(&limit) {
            return Err(IndexError::InvalidRequest(
                "limit must be between 1 and 100".into(),
            ));
        }
        let words = tokenizer::query_words(query)?;
        let exact_symbols = tokenizer::exact_symbols(query);
        if words.is_empty() && exact_symbols.is_empty() {
            return Err(IndexError::InvalidRequest(
                "query must contain a searchable term".into(),
            ));
        }
        Ok(Self {
            words,
            exact_symbols,
            vendor: None,
            document_id: None,
            kind: None,
            limit,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LexicalHit {
    pub chunk_id: String,
    pub score: f32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LexicalBuildStats {
    pub document_count: u64,
    pub component_checksum: String,
}

#[derive(Debug, Error)]
pub enum VectorError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid vector file magic")]
    InvalidMagic,
    #[error("unsupported vector format version {0}")]
    UnsupportedVersion(u32),
    #[error("vector dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("vector payload length mismatch: header {header}, computed {computed}")]
    PayloadLengthMismatch { header: u64, computed: u64 },
    #[error("vector file length mismatch: expected {expected}, got {actual}")]
    FileLengthMismatch { expected: u64, actual: u64 },
    #[error("vector payload hash mismatch")]
    PayloadHashMismatch,
    #[error("non-finite vector value at row {row}, column {column}")]
    NonFiniteValue { row: u64, column: usize },
    #[error("zero-norm vector at row {row:?}")]
    ZeroNorm { row: Option<u64> },
    #[error("non-unit vector at row {row}")]
    NonUnitVector { row: u64 },
    #[error("vector dimensions or counts overflow the file format")]
    SizeOverflow,
    #[error("top-k must be between 1 and the vector count")]
    InvalidTopK,
    #[error("allowed row {row} is outside vector count {count}")]
    InvalidAllowedRow { row: u64, count: u64 },
    #[error("allowed rows must be strictly increasing")]
    UnsortedAllowedRows,
    #[error("vector payload is not aligned for f32 access")]
    UnalignedPayload,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct VectorBuildStats {
    pub count: u64,
    pub dimension: usize,
    pub payload_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct VectorHit {
    pub row: u64,
    pub score: f32,
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::domain::{
        Vendor,
        block::ContentClass,
        chunk::{ChunkKind, SearchChunk},
        source::SourceSpan,
    };

    use super::{
        LexicalSearchRequest, LexicalSearcher, LexicalWriter, normalize_symbol, symbol_terms,
    };

    fn chunk(
        id: &str,
        vendor: Vendor,
        kind: ChunkKind,
        heading: &str,
        text: &str,
        symbols: &[&str],
    ) -> SearchChunk {
        SearchChunk {
            chunk_id: id.into(),
            document_id: format!("doc:{vendor:?}"),
            section_id: format!("sec:{id}"),
            vendor,
            heading_path: vec![heading.into()],
            source_block_ids: vec![format!("blk:{id}")],
            kind,
            content_class: ContentClass::Substantive,
            text: text.into(),
            token_count: text.split_whitespace().count() as u32,
            content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            symbols: symbols.iter().map(|symbol| (*symbol).into()).collect(),
            span: SourceSpan {
                byte_start: 0,
                byte_end: text.len() as u64,
                line_start: 1,
                line_end: 1,
                printed_page: Some("26-1".into()),
            },
        }
    }

    fn fixture_chunks() -> Vec<SearchChunk> {
        vec![
            chunk(
                "chk:exact",
                Vendor::Intel,
                ChunkKind::Prose,
                "Extended Feature Enable Register",
                "IA32_EFER controls extended processor features.",
                &["IA32_EFER"],
            ),
            chunk(
                "chk:component",
                Vendor::Intel,
                ChunkKind::Prose,
                "EFER overview",
                "The EFER register is described here.",
                &["EFER"],
            ),
            chunk(
                "chk:vmx",
                Vendor::Intel,
                ChunkKind::Code,
                "Virtual-machine extensions",
                "Set CR4.VMXE to enable virtual-machine extensions.",
                &["CR4.VMXE", "CPUID.07H.00H:EBX"],
            ),
            chunk(
                "chk:amd-pf",
                Vendor::Amd,
                ChunkKind::Prose,
                "Page-fault exception",
                "A page fault raises #PF while translating a linear address.",
                &["#PF"],
            ),
        ]
    }

    fn searcher() -> LexicalSearcher {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("lexical");
        let stats = LexicalWriter::create(&path)
            .unwrap()
            .write(&fixture_chunks())
            .unwrap();
        assert_eq!(stats.document_count, 4);
        assert!(!stats.component_checksum.is_empty());
        let searcher = LexicalSearcher::open(&path).unwrap();
        std::mem::forget(temporary);
        searcher
    }

    #[test]
    fn normalizes_complete_x86_symbols_and_unique_components() {
        assert_eq!(normalize_symbol("(ia32_efer),"), "IA32_EFER");
        assert_eq!(normalize_symbol("`CPUID.07H.00H:EBX`"), "CPUID.07H.00H:EBX");
        assert_eq!(normalize_symbol("#pf"), "#PF");
        assert_eq!(normalize_symbol("RFLAGS[11:8]."), "RFLAGS[11:8]");
        assert_eq!(symbol_terms("#PF"), vec!["#PF", "PF"]);
        assert_eq!(
            symbol_terms("CPUID.07H.00H:EBX"),
            vec!["CPUID.07H.00H:EBX", "CPUID", "07H", "00H", "EBX"]
        );
        assert_eq!(symbol_terms("CR4.VMXE"), vec!["CR4.VMXE", "CR4", "VMXE"]);
    }

    #[test]
    fn exact_symbol_ranks_before_component_match() {
        let hits = searcher()
            .search(&LexicalSearchRequest::from_query("IA32_EFER", 10).unwrap())
            .unwrap();
        assert_eq!(hits[0].chunk_id, "chk:exact");
        assert!(hits.iter().any(|hit| hit.chunk_id == "chk:component"));
    }

    #[test]
    fn bm25_finds_prose_and_filters_vendor_and_kind() {
        let searcher = searcher();
        let hits = searcher
            .search(
                &LexicalSearchRequest::from_query("enable virtual machine extensions", 10).unwrap(),
            )
            .unwrap();
        assert_eq!(hits[0].chunk_id, "chk:vmx");

        let mut filtered = LexicalSearchRequest::from_query("page fault", 10).unwrap();
        filtered.vendor = Some(Vendor::Amd);
        filtered.kind = Some(ChunkKind::Prose);
        let hits = searcher.search(&filtered).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, "chk:amd-pf");
        filtered.vendor = Some(Vendor::Intel);
        assert!(searcher.search(&filtered).unwrap().is_empty());
        filtered.vendor = Some(Vendor::Amd);
        filtered.kind = Some(ChunkKind::Code);
        assert!(searcher.search(&filtered).unwrap().is_empty());
    }

    #[test]
    fn raw_field_syntax_cannot_select_filter_fields() {
        let hits = searcher()
            .search(&LexicalSearchRequest::from_query("vendor:intel", 10).unwrap())
            .unwrap();
        assert!(hits.is_empty());
    }
}

#[cfg(test)]
mod vector_tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{VectorError, VectorReader, VectorWriter};

    const HEADER_LEN: usize = 64;

    fn valid_store(directory: &Path) -> std::path::PathBuf {
        let path = directory.join("vectors.f32");
        VectorWriter::write(
            &path,
            3,
            &[
                vec![1.0, 0.0, 0.0],
                vec![0.0, 1.0, 0.0],
                vec![1.0, 1.0, 0.0],
            ],
        )
        .unwrap();
        path
    }

    fn corrupt(source: &Path, destination: &Path, mutate: impl FnOnce(&mut Vec<u8>)) {
        let mut bytes = fs::read(source).unwrap();
        mutate(&mut bytes);
        fs::write(destination, bytes).unwrap();
    }

    #[test]
    fn mmap_vectors_rank_exact_cosine_with_optional_rows() {
        let temporary = tempdir().unwrap();
        let path = valid_store(temporary.path());
        let reader = VectorReader::open(&path, 3).unwrap();
        assert_eq!(reader.count(), 3);
        assert_eq!(reader.dimension(), 3);

        let hits = reader.top_k(&[1.0, 0.2, 0.0], None, 3).unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.row).collect::<Vec<_>>(),
            vec![0, 2, 1]
        );
        let filtered = reader.top_k(&[1.0, 0.2, 0.0], Some(&[1, 2]), 2).unwrap();
        assert_eq!(
            filtered.iter().map(|hit| hit.row).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let tied = reader.top_k(&[0.0, 0.0, 1.0], None, 3).unwrap();
        assert_eq!(
            tied.iter().map(|hit| hit.row).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn rejects_corrupt_vector_headers_and_payloads() {
        let temporary = tempdir().unwrap();
        let valid = valid_store(temporary.path());

        let magic = temporary.path().join("magic.f32");
        corrupt(&valid, &magic, |bytes| bytes[0] ^= 0xff);
        assert!(matches!(
            VectorReader::open(&magic, 3),
            Err(VectorError::InvalidMagic)
        ));

        let dimension = temporary.path().join("dimension.f32");
        corrupt(&valid, &dimension, |bytes| {
            bytes[12..16].copy_from_slice(&4_u32.to_le_bytes())
        });
        assert!(matches!(
            VectorReader::open(&dimension, 3),
            Err(VectorError::DimensionMismatch {
                expected: 3,
                actual: 4
            })
        ));

        let count = temporary.path().join("count.f32");
        corrupt(&valid, &count, |bytes| {
            bytes[16..24].copy_from_slice(&4_u64.to_le_bytes())
        });
        assert!(matches!(
            VectorReader::open(&count, 3),
            Err(VectorError::PayloadLengthMismatch { .. })
        ));

        let truncated = temporary.path().join("truncated.f32");
        corrupt(&valid, &truncated, |bytes| {
            bytes.pop();
        });
        assert!(matches!(
            VectorReader::open(&truncated, 3),
            Err(VectorError::FileLengthMismatch { .. })
        ));

        let hash = temporary.path().join("hash.f32");
        corrupt(&valid, &hash, |bytes| bytes[HEADER_LEN] ^= 1);
        assert!(matches!(
            VectorReader::open(&hash, 3),
            Err(VectorError::PayloadHashMismatch)
        ));

        let nan = temporary.path().join("nan.f32");
        corrupt(&valid, &nan, |bytes| {
            bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&f32::NAN.to_le_bytes());
            let hash = blake3::hash(&bytes[HEADER_LEN..]);
            bytes[32..64].copy_from_slice(hash.as_bytes());
        });
        assert!(matches!(
            VectorReader::open(&nan, 3),
            Err(VectorError::NonFiniteValue { row: 0, column: 0 })
        ));
    }
}
