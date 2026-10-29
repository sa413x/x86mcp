mod build_context;
mod compare_vendors;
mod get_diagram;
mod get_outline;
mod get_references;
mod get_section;
mod get_table;
mod index_status;
mod lookup;
mod search;

use rmcp::{
    ErrorData,
    handler::server::wrapper::{Json, Parameters},
    tool, tool_router,
};

use crate::query::{
    BuildContextRequest, BuildContextResponse, CompareVendorsRequest, CompareVendorsResponse,
    GetDiagramRequest, GetDiagramResponse, GetOutlineRequest, GetOutlineResponse,
    GetReferencesRequest, GetReferencesResponse, GetSectionRequest, GetSectionResponse,
    GetTableRequest, GetTableResponse, IndexStatusResponse, LookupRequest, LookupResponse,
    SearchRequest, SearchResponse,
};

use super::server::X86McpServer;

#[tool_router(vis = "pub(crate)")]
impl X86McpServer {
    #[tool(
        name = "x86_search",
        description = "Search Intel SDM and AMD APM evidence with exact-symbol, BM25 lexical, semantic, or hybrid retrieval. Returns deterministic scores, citations, snapshot state, truncation disclosure, and a snapshot-bound cursor.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_search(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, ErrorData> {
        search::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_lookup",
        description = "Look up a typed x86 entity such as an instruction, MSR, CPUID leaf, register, exception, bitfield, or term. Exact-symbol hits are returned as exact evidence only when they match the requested kind (instruction: reference heading begins with the mnemonic; msr: IA32_*/MSR_* name, hex address, or MSR mention; cpuid: CPUID leaf reference with CPUID mention; register: architectural register name or register mention; exception: #XX mnemonic or exception mention; bitfield: REG.FIELD/REG[bits] or bit/flag/field mention); mismatched hits are reported as related. entity_state is found only with kind-matched exact evidence, except term, which is lexical only. Returns cited evidence with truncation state.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_lookup(
        &self,
        Parameters(request): Parameters<LookupRequest>,
    ) -> Result<Json<LookupResponse>, ErrorData> {
        lookup::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_get_section",
        description = "Retrieve a parsed manual section by stable section ID, including source blocks, tables, diagrams, citations, and a snapshot-bound continuation cursor.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_get_section(
        &self,
        Parameters(request): Parameters<GetSectionRequest>,
    ) -> Result<Json<GetSectionResponse>, ErrorData> {
        get_section::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_get_outline",
        description = "List documents or walk a document section hierarchy from an optional root, with bounded depth, stable IDs, per-section source citations, and cursor pagination.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_get_outline(
        &self,
        Parameters(request): Parameters<GetOutlineRequest>,
    ) -> Result<Json<GetOutlineResponse>, ErrorData> {
        get_outline::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_get_table",
        description = "Retrieve a normalized Intel or AMD manual table by table ID or containing block ID. Supports row filtering and offset pagination while preserving raw source and persisted extraction warnings.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_get_table(
        &self,
        Parameters(request): Parameters<GetTableRequest>,
    ) -> Result<Json<GetTableResponse>, ErrorData> {
        get_table::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_get_diagram",
        description = "Retrieve a parsed Mermaid or text diagram by diagram ID or containing block ID, including nodes, edges, subgraphs, raw source, warnings, nearby blocks, and citation.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_get_diagram(
        &self,
        Parameters(request): Parameters<GetDiagramRequest>,
    ) -> Result<Json<GetDiagramResponse>, ErrorData> {
        get_diagram::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_get_references",
        description = "Traverse resolved and unresolved manual cross-references incoming to or outgoing from a block, section, document, table, or diagram ID; table and diagram IDs and their containing block IDs are interchangeable. Each reference carries a source citation. Reports ambiguity and whether more results exist.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_get_references(
        &self,
        Parameters(request): Parameters<GetReferencesRequest>,
    ) -> Result<Json<GetReferencesResponse>, ErrorData> {
        get_references::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_compare_vendors",
        description = "Run the same bounded retrieval independently for Intel and AMD, returning balanced per-vendor cited evidence and per-side truncation disclosure.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_compare_vendors(
        &self,
        Parameters(request): Parameters<CompareVendorsRequest>,
    ) -> Result<Json<CompareVendorsResponse>, ErrorData> {
        compare_vendors::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_build_context",
        description = "Assemble a citation-preserving, token-budgeted context pack from a query and/or explicit chunk IDs. The vendors filter applies to both. Merges only adjacent source chunks and reports every omission (missing ID, excluded vendor, or exhausted budget).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_build_context(
        &self,
        Parameters(request): Parameters<BuildContextRequest>,
    ) -> Result<Json<BuildContextResponse>, ErrorData> {
        build_context::invoke(self, request).await.map(Json)
    }

    #[tool(
        name = "x86_index_status",
        description = "Report current snapshot identity, readiness, staleness or model degradation reasons, per-archive checksum freshness, model contract, component checksums, and indexed entity counts. Works before any data is installed: state is then missing or invalid, reasons say why, and manifest and counts are null.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn x86_index_status(&self) -> Result<Json<IndexStatusResponse>, ErrorData> {
        index_status::invoke(self).await.map(Json)
    }
}
