use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::source::SourceSpan;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Prose,
    List,
    Code,
    Table,
    Diagram,
    Quote,
    Caption,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentClass {
    Substantive,
    FrontMatter,
    Contents,
    Legal,
    RevisionHistory,
    PageFurniture,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SourceBlock {
    pub block_id: String,
    pub document_id: String,
    pub section_id: String,
    pub kind: BlockKind,
    pub heading_path: Vec<String>,
    pub raw_source: String,
    pub normalized_text: String,
    pub content_class: ContentClass,
    pub span: SourceSpan,
}
