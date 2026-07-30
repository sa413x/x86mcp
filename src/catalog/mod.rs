mod reader;
pub(crate) mod schema;
mod writer;

use std::{io, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{
        Vendor, block::SourceBlock, chunk::ChunkKind, document::DocumentMeta,
        reference::ReferenceRecord,
    },
    ingest::{ExtractedDiagram, SectionNode},
};

pub use reader::CatalogReader;
pub use writer::CatalogWriter;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SQLite catalog error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("catalog JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog integrity error: {0}")]
    Integrity(String),
    #[error("unsupported catalog schema {found}; supported schema is {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("invalid catalog request: {0}")]
    InvalidRequest(String),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CatalogDocument {
    pub meta: DocumentMeta,
    pub title: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct OutlineNode {
    pub section: SectionNode,
    pub relative_depth: u8,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SectionView {
    pub section: SectionNode,
    pub blocks: Vec<SourceBlock>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct TablePage {
    pub table_id: String,
    pub block_id: String,
    pub caption: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: u32,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    pub raw_source: String,
}

pub type DiagramData = ExtractedDiagram;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceDirection {
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ResolvedReference {
    pub record: ReferenceRecord,
    pub source_document_id: String,
    pub source_section_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CatalogCounts {
    pub archives: u64,
    pub documents: u64,
    pub sections: u64,
    pub blocks: u64,
    pub chunks: u64,
    pub tables: u64,
    pub diagrams: u64,
    pub references: u64,
    pub warnings: u64,
    pub vectors: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct VectorReuseRecord {
    pub content_hash: String,
    pub vector_row: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct VectorChunkMetadata {
    pub vector_row: u64,
    pub chunk_id: String,
    pub document_id: String,
    pub vendor: Vendor,
    pub kind: ChunkKind,
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::{
        domain::{
            Vendor,
            document::{ArchiveDocument, DocumentMeta},
        },
        ingest::{ChunkConfig, IngestWarning, TokenCounter, chunk_document, parse_document},
    };

    use super::{CatalogReader, CatalogWriter, ReferenceDirection, schema::SCHEMA_SQL};

    struct Words;
    impl TokenCounter for Words {
        fn count(&self, text: &str) -> Result<usize> {
            Ok(text.split_whitespace().count())
        }
    }

    fn document(source: &str) -> ArchiveDocument {
        ArchiveDocument {
            meta: DocumentMeta {
                document_id: "doc:catalog".into(),
                vendor: Vendor::Intel,
                archive_id: "intel".into(),
                archive_sha256: "0".repeat(64),
                entry_path: "manual.md".into(),
                content_sha256: "1".repeat(64),
                byte_len: source.len() as u64,
            },
            source: source.into(),
        }
    }

    #[test]
    fn round_trips_linked_manual_structures() {
        let source = "# CHAPTER 26 VMX\n\n## 26.1 OVERVIEW\n\nSee Section 26.1 and Figure 26-1.\n\n| Bit | Meaning |\n| --- | --- |\n| 13 | VMXE |\n| 0 | Lock |\n\nTable 26-1. VMX controls\n\n```mermaid\ngraph TD\nVMM --> G1[Guest 1]\n```\n\nFigure 26-1. VMM and guest\n";
        let document = document(source);
        let mut parsed = parse_document(&document).unwrap();
        parsed.warnings.push(IngestWarning {
            code: "fixture_warning".into(),
            message: "retained conversion artifact".into(),
            span: parsed.blocks[0].span.clone(),
        });
        let chunks = chunk_document(
            &document,
            &parsed,
            &Words,
            ChunkConfig {
                target_tokens: 32,
                table_rows_per_chunk: 1,
                ..ChunkConfig::default()
            },
        )
        .unwrap();

        let temporary = tempdir().unwrap();
        let catalog_path = temporary.path().join("catalog.sqlite3");
        let mut writer = CatalogWriter::create(&catalog_path).unwrap();
        let next_vector_row = writer
            .write_document(&document, &parsed, &chunks, 0)
            .unwrap();
        assert_eq!(next_vector_row, chunks.len() as u64);
        writer.finish().unwrap();

        let reader = CatalogReader::open(&catalog_path).unwrap();
        assert_eq!(
            reader
                .parsed_document(&document.meta.document_id)
                .unwrap()
                .unwrap(),
            parsed
        );
        let section = reader
            .section(&parsed.sections[1].section_id)
            .unwrap()
            .unwrap();
        assert!(section.blocks.iter().any(|block| {
            block.raw_source == "| Bit | Meaning |\n| --- | --- |\n| 13 | VMXE |\n| 0 | Lock |\n"
        }));

        let table = reader
            .table(&parsed.tables[0].source_block_id, 0, 1)
            .unwrap()
            .unwrap();
        assert_eq!(table.headers, vec!["Bit", "Meaning"]);
        assert_eq!(table.rows, vec![vec!["13", "VMXE"]]);
        assert_eq!(table.total_rows, 2);
        assert!(table.has_more);

        let diagram = reader
            .diagram(&parsed.diagrams[0].source_block_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            diagram.caption.as_deref(),
            Some("Figure 26-1. VMM and guest")
        );
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| edge.from == "VMM" && edge.to == "G1")
        );

        let references = reader
            .references(
                &parsed.references[0].source_block_id,
                ReferenceDirection::Outgoing,
                20,
            )
            .unwrap();
        assert_eq!(references.len(), 2);
        assert!(references.iter().any(|reference| reference.record.resolved));
        assert_eq!(reader.warning_count().unwrap(), 1);
        assert_eq!(reader.vector_row(&chunks[0].chunk_id).unwrap(), Some(0));
        assert_eq!(
            reader.chunk(&chunks[0].chunk_id).unwrap().unwrap(),
            chunks[0]
        );
    }

    #[test]
    fn schema_rejects_foreign_key_invalid_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(SCHEMA_SQL).unwrap();
        let error = connection
            .execute(
                "INSERT INTO sections(id, document_id, parent_id, heading, heading_path_json, level, ordinal, printed_page, byte_start, byte_end, line_start, line_end) VALUES ('sec:bad', 'doc:missing', NULL, 'bad', '[]', 1, 1, NULL, 0, 1, 1, 1)",
                [],
            )
            .unwrap_err();
        assert!(matches!(error, rusqlite::Error::SqliteFailure(_, _)));
    }
}
