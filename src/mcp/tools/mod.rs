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
    ErrorData, ServerHandler,
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};

use crate::query::{
    BuildContextRequest, BuildContextResponse, CompareVendorsRequest, CompareVendorsResponse,
    GetDiagramRequest, GetDiagramResponse, GetOutlineRequest, GetOutlineResponse,
    GetReferencesRequest, GetReferencesResponse, GetSectionRequest, GetSectionResponse,
    GetTableRequest, GetTableResponse, IndexStatusResponse, LookupRequest, LookupResponse,
    SearchRequest, SearchResponse,
};

use super::server::X86McpServer;

#[tool_router]
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
        description = "Look up a typed x86 entity such as an instruction, MSR, CPUID leaf, register, exception, bitfield, or term. Returns exact and related cited evidence with explicit not-found and truncation state.",
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
        description = "List documents or walk a document section hierarchy from an optional root, with bounded depth, stable IDs, citations, and cursor pagination.",
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
        description = "Retrieve a normalized Intel or AMD manual table by table ID or containing block ID. Supports row filtering and offset pagination while preserving raw source and extraction warnings.",
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
        description = "Traverse resolved and unresolved manual cross-references incoming to or outgoing from a block, section, document, table, or diagram ID. Reports ambiguity and whether more results exist.",
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
        description = "Assemble a citation-preserving, token-budgeted context pack from a query and/or explicit chunk IDs. Merges only adjacent source chunks and reports every budget omission.",
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
        description = "Report current snapshot identity, readiness, staleness or model degradation reasons, per-archive checksum freshness, model contract, component checksums, and indexed entity counts.",
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

#[tool_handler]
impl ServerHandler for X86McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("x86mcp", env!("CARGO_PKG_VERSION"))
                    .with_title("x86 Architecture Manuals"),
            )
            .with_instructions(
                "Use x86_index_status before retrieval when freshness matters. Prefer x86_lookup for named architectural entities, x86_search for open-ended evidence, and x86_build_context for bounded prompts. Preserve returned citations and disclose stale or semantic_degraded_reason state.",
            )
    }
}
