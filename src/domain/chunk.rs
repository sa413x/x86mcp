use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Vendor, block::ContentClass, source::SourceSpan};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Prose,
    List,
    Code,
    Table,
    Diagram,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub section_id: String,
    pub vendor: Vendor,
    pub heading_path: Vec<String>,
    pub source_block_ids: Vec<String>,
    pub kind: ChunkKind,
    pub content_class: ContentClass,
    pub text: String,
    pub token_count: u32,
    pub content_hash: String,
    pub symbols: Vec<String>,
    pub span: SourceSpan,
}
