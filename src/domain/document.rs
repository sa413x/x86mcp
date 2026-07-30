use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Vendor, source::SourceSpan};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct DocumentMeta {
    pub document_id: String,
    pub vendor: Vendor,
    pub archive_id: String,
    pub archive_sha256: String,
    pub entry_path: String,
    pub content_sha256: String,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ArchiveDocument {
    pub meta: DocumentMeta,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SectionRecord {
    pub section_id: String,
    pub document_id: String,
    pub level: u8,
    pub title: String,
    pub heading_path: Vec<String>,
    pub span: SourceSpan,
    pub parent_section_id: Option<String>,
}
