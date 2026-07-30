use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    catalog::{
        CatalogCounts, CatalogDocument, DiagramData, OutlineNode, ReferenceDirection,
        ResolvedReference, SectionView, TablePage,
    },
    domain::{Vendor, block::SourceBlock, chunk::ChunkKind, citation::Citation},
    snapshot::{SnapshotError, SnapshotManifest, SnapshotStatus},
};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("invalid query input: {0}")]
    InvalidInput(String),
    #[error("catalog query failed: {0}")]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error("lexical query failed: {0}")]
    Index(#[from] crate::index::IndexError),
    #[error("vector query failed: {0}")]
    Vector(#[from] crate::index::VectorError),
    #[error("semantic query failed: {0}")]
    Semantic(String),
    #[error("snapshot query failed: {0}")]
    Snapshot(#[from] SnapshotError),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Exact,
    Lexical,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub vendors: Vec<Vendor>,
    #[serde(default)]
    pub document_ids: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<ChunkKind>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ScoreBreakdown {
    pub exact_rank: Option<u32>,
    pub lexical_rank: Option<u32>,
    pub lexical_score: Option<f32>,
    pub semantic_rank: Option<u32>,
    pub semantic_score: Option<f32>,
    pub rrf_score: f32,
    pub boost: f32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchHit {
    pub chunk_id: String,
    pub snippet: String,
    pub citation: Citation,
    pub scores: ScoreBreakdown,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct RetrievalState {
    pub snapshot_id: String,
    pub stale: bool,
    pub semantic_degraded_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchResponse {
    pub state: RetrievalState,
    pub hits: Vec<SearchHit>,
    pub next_cursor: Option<String>,
    pub candidate_window_truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Instruction,
    Msr,
    Cpuid,
    Register,
    Exception,
    Bitfield,
    Term,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityState {
    Found,
    NotFound,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LookupRequest {
    pub entity: String,
    pub kind: EntityKind,
    #[serde(default)]
    pub vendors: Vec<Vendor>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LookupResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub entity: String,
    pub kind: EntityKind,
    pub exact: Vec<SearchHit>,
    pub related: Vec<SearchHit>,
    pub exact_truncated: bool,
    pub related_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetSectionRequest {
    pub id: String,
    pub block_limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub include_neighbors: bool,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SectionNeighbors {
    pub previous: Option<crate::ingest::SectionNode>,
    pub next: Option<crate::ingest::SectionNode>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetSectionResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub section: Option<SectionView>,
    pub block: Option<SourceBlock>,
    pub citation: Option<Citation>,
    pub children: Vec<OutlineNode>,
    pub neighbors: SectionNeighbors,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetOutlineRequest {
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub root_section_id: Option<String>,
    pub depth: u8,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetOutlineResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub documents: Vec<CatalogDocument>,
    pub nodes: Vec<OutlineNode>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetTableRequest {
    pub id: String,
    pub offset: u32,
    pub limit: u32,
    #[serde(default)]
    pub row_filter: Option<String>,
    #[serde(default)]
    pub include_raw: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetTableResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub table: Option<TablePage>,
    pub citation: Option<Citation>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetDiagramRequest {
    pub id: String,
    #[serde(default = "default_true")]
    pub include_raw: bool,
    #[serde(default)]
    pub include_surrounding: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetDiagramResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub diagram: Option<DiagramData>,
    pub citation: Option<Citation>,
    pub surrounding: Vec<SourceBlock>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetReferencesRequest {
    pub id: String,
    pub direction: ReferenceDirection,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct GetReferencesResponse {
    pub state: RetrievalState,
    pub entity_state: EntityState,
    pub references: Vec<ResolvedReference>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CompareVendorsRequest {
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    pub limit_per_vendor: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CompareVendorsResponse {
    pub state: RetrievalState,
    pub intel: Vec<SearchHit>,
    pub amd: Vec<SearchHit>,
    pub intel_truncated: bool,
    pub amd_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct BuildContextRequest {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub chunk_ids: Vec<String>,
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub vendors: Vec<Vendor>,
    pub token_budget: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ContextItem {
    pub text: String,
    pub chunk_ids: Vec<String>,
    pub citations: Vec<Citation>,
    pub estimated_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ContextOmission {
    pub chunk_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct BuildContextResponse {
    pub state: RetrievalState,
    pub items: Vec<ContextItem>,
    pub omitted: Vec<ContextOmission>,
    pub estimated_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct IndexStatusResponse {
    pub status: SnapshotStatus,
    pub manifest: SnapshotManifest,
    pub counts: CatalogCounts,
}

fn default_true() -> bool {
    true
}
