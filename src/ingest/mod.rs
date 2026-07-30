mod chunker;
mod diagrams;
mod headings;
mod markdown;
mod normalize;
mod references;
mod tables;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{block::SourceBlock, reference::ReferenceRecord, source::SourceSpan};

pub use chunker::{ChunkConfig, TokenCounter, chunk_document};
pub use diagrams::{DiagramEdge, DiagramError, DiagramNode, ExtractedDiagram, extract_diagram};
pub use markdown::parse_document;
pub use references::{extract_references, resolve_corpus_references};
pub use tables::{ExtractedTable, TableError, extract_table};

pub const INGEST_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ParsedDocument {
    pub document_id: String,
    pub sections: Vec<SectionNode>,
    pub blocks: Vec<SourceBlock>,
    pub tables: Vec<ExtractedTable>,
    pub diagrams: Vec<ExtractedDiagram>,
    pub references: Vec<ReferenceRecord>,
    pub warnings: Vec<IngestWarning>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SectionNode {
    pub section_id: String,
    pub parent_section_id: Option<String>,
    pub level: u8,
    pub heading: String,
    pub heading_path: Vec<String>,
    pub ordinal: u32,
    pub span: SourceSpan,
    pub printed_page: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct IngestWarning {
    pub code: String,
    pub message: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ExtractionWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("invalid document structure: {reason}")]
    InvalidDocument { reason: String },
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::{
            Vendor,
            block::{BlockKind, ContentClass},
            document::{ArchiveDocument, DocumentMeta},
        },
        ingest::parse_document,
    };

    fn document(source: &str) -> ArchiveDocument {
        ArchiveDocument {
            meta: DocumentMeta {
                document_id: "doc:test".into(),
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
    fn parses_observed_conversion_shapes_without_rebuilding_source() {
        let source = "Intel logo\n\n# Intel® 64 and IA-32 Architectures Software Developer’s Manual\n\n# Notices & Disclaimers\n\nNo product can be absolutely secure.\n\n# CHAPTER 26 INTRODUCTION TO VIRTUAL MACHINE EXTENSIONS\n\n## 26.1 OVERVIEW\n\nVMX preserves CR4.VMXE and IA32_FEATURE_CONTROL.\n\n```text\nVMXON\n```\n\nVol. 3C\n<page_number>\n26-1\n</page_number>\nINTRODUCTION TO VIRTUAL MACHINE EXTENSIONS\n";
        let document = document(source);
        let parsed = parse_document(&document).unwrap();

        assert_eq!(
            parsed.sections[0].heading,
            "CHAPTER 26 INTRODUCTION TO VIRTUAL MACHINE EXTENSIONS"
        );
        assert_eq!(parsed.sections[0].printed_page.as_deref(), Some("26-1"));
        assert!(
            parsed
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::Code)
        );
        assert!(parsed.blocks.iter().all(|block| {
            block.raw_source
                == document.source[block.span.byte_start as usize..block.span.byte_end as usize]
        }));
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn duplicate_headings_receive_distinct_ordinals_and_ids() {
        let parsed = parse_document(&document(
            "# CHAPTER 1 TEST\n\n## 1.1 SAME\nFirst.\n\n## 1.1 SAME\nSecond.\n",
        ))
        .unwrap();
        let duplicates = parsed
            .sections
            .iter()
            .filter(|section| section.heading == "1.1 SAME")
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);
        assert_eq!(duplicates[0].ordinal, 1);
        assert_eq!(duplicates[1].ordinal, 2);
        assert_ne!(duplicates[0].section_id, duplicates[1].section_id);
    }

    #[test]
    fn crlf_ranges_use_byte_offsets_and_one_based_lines() {
        let source = "# CHAPTER 1 UTF-8\r\n\r\n## 1.1 CAFÉ\r\n\r\nCR4.VMXE is set.\r\n";
        let document = document(source);
        let parsed = parse_document(&document).unwrap();
        let prose = parsed
            .blocks
            .iter()
            .find(|block| block.normalized_text.contains("CR4.VMXE"))
            .unwrap();
        assert_eq!(prose.span.line_start, 5);
        assert_eq!(prose.span.line_end, 5);
        assert_eq!(
            prose.raw_source,
            document.source[prose.span.byte_start as usize..prose.span.byte_end as usize]
        );
    }

    #[test]
    fn front_matter_is_ranked_instead_of_deleted() {
        let parsed = parse_document(&document(
            "Intel logo\n\n# Intel® Manual\n\n# Notices & Disclaimers\n\nNo product can be absolutely secure.\n\n# CHAPTER 1 START\nUseful text.\n",
        ))
        .unwrap();
        assert!(parsed.blocks.iter().any(|block| matches!(
            block.content_class,
            ContentClass::FrontMatter | ContentClass::Legal
        )));
        assert!(
            parsed
                .blocks
                .iter()
                .any(|block| block.content_class == ContentClass::Substantive)
        );
    }

    #[test]
    fn unclosed_fence_is_retained_and_reported() {
        let source = "# CHAPTER 1 TEST\n\n```text\nMOV EAX, CR4\n";
        let document = document(source);
        let parsed = parse_document(&document).unwrap();
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.code == "unclosed_fence")
        );
        let code = parsed
            .blocks
            .iter()
            .find(|block| block.kind == BlockKind::Code)
            .unwrap();
        assert_eq!(code.raw_source, "```text\nMOV EAX, CR4\n");
    }

    #[test]
    fn parsed_document_attaches_captions_and_resolves_local_references() {
        let parsed = parse_document(&document(
            "# CHAPTER 26 VMX\n\nSee Figure 26-1.\n\n```mermaid\ngraph TD\nVMM --> G1[Guest 1]\n```\n\nFigure 26-1. VMM and guest\n",
        ))
        .unwrap();
        assert_eq!(parsed.diagrams.len(), 1);
        assert_eq!(
            parsed.diagrams[0].caption.as_deref(),
            Some("Figure 26-1. VMM and guest")
        );
        assert_eq!(parsed.references.len(), 1);
        assert!(parsed.references[0].resolved);
        assert_eq!(
            parsed.references[0].target_id.as_deref(),
            Some(parsed.diagrams[0].diagram_id.as_str())
        );
    }
}
