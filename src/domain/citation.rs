use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Vendor, source::SourceSpan};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct Citation {
    pub vendor: Vendor,
    pub document_id: String,
    pub entry_path: String,
    pub section_id: String,
    pub heading_path: Vec<String>,
    pub span: SourceSpan,
    pub source_url: Option<String>,
}
