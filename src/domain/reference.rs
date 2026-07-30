use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Section,
    Table,
    Figure,
    Document,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ReferenceRecord {
    pub reference_id: String,
    pub source_block_id: String,
    pub kind: ReferenceKind,
    pub label: String,
    pub normalized_key: String,
    pub target_document_id: Option<String>,
    pub target_id: Option<String>,
    pub candidates: Vec<String>,
    pub resolved: bool,
}
