use std::sync::Arc;

use anyhow::Context;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        Implementation, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerInfo,
    },
    prompt_handler,
    service::RequestContext,
    tool_handler,
};
use tokio::sync::OnceCell;

use crate::{
    config::AppConfig,
    query::{IndexStatusResponse, QueryEngine, QueryError},
    snapshot::{Snapshot, SnapshotState, SnapshotStatus, StatusMonitor},
};

use super::{
    resources,
    types::{join_error, query_error},
};

/// MCP server over the index under `config.root`.
///
/// The index is opened on first use instead of at startup, so the handshake
/// and the tool, prompt, and resource listings work before `x86mcp setup` has
/// installed any data. Until a usable index exists, `x86_index_status`
/// reports why and every other request fails with setup instructions; data
/// installed while the server runs is picked up by the next request. Once
/// opened, the engine is kept for the life of the server.
#[derive(Clone)]
pub struct X86McpServer {
    config: Arc<AppConfig>,
    engine: Arc<OnceCell<Arc<QueryEngine>>>,
}

impl X86McpServer {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Arc::new(config),
            engine: Arc::new(OnceCell::new()),
        }
    }

    /// The query engine, opening the index on the first call that finds it
    /// usable. `Err` is the status explaining why no usable index exists.
    pub(crate) async fn engine(&self) -> Result<Arc<QueryEngine>, Box<SnapshotStatus>> {
        let config = Arc::clone(&self.config);
        self.engine
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || open_engine(&config))
                    .await
                    .map_err(|error| invalid(format!("index loader failed: {error}")))?
                    .map(Arc::new)
            })
            .await
            .cloned()
    }

    pub(crate) async fn execute<T, F>(&self, operation: F) -> Result<T, ErrorData>
    where
        T: Send + 'static,
        F: FnOnce(&QueryEngine) -> Result<T, QueryError> + Send + 'static,
    {
        let engine = self
            .engine()
            .await
            .map_err(|status| self.unavailable(&status))?;
        tokio::task::spawn_blocking(move || operation(&engine))
            .await
            .map_err(join_error)?
            .map_err(query_error)
    }

    /// Status of the opened index, or of the installation when none is usable.
    pub(crate) async fn index_status(&self) -> Result<IndexStatusResponse, ErrorData> {
        match self.engine().await {
            Ok(engine) => tokio::task::spawn_blocking(move || engine.index_status())
                .await
                .map_err(join_error),
            Err(status) => Ok(IndexStatusResponse {
                status: *status,
                manifest: None,
                counts: None,
            }),
        }
    }

    /// The error returned instead of retrieval results while no usable index exists.
    fn unavailable(&self, status: &SnapshotStatus) -> ErrorData {
        let state = serde_json::to_value(status.state)
            .ok()
            .and_then(|state| state.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{:?}", status.state));
        ErrorData::internal_error(
            format!(
                "the x86 manual index at {} is {state}: {}. Install the Intel SDM/AMD APM data with `x86mcp setup` (`npx -y x86mcp setup` when the server runs through npm), passing the same --root as this server if it uses one, or build a source checkout with `x86mcp index`. This server picks up the data on the next request without a restart.",
                self.config.root.display(),
                status.reasons.join("; "),
            ),
            None,
        )
    }
}

fn open_engine(config: &AppConfig) -> Result<QueryEngine, Box<SnapshotStatus>> {
    let status = StatusMonitor::new(config);
    let current = status.current_status();
    if !current.is_usable() {
        return Err(Box::new(current));
    }
    let snapshot = Snapshot::open(&config.index_dir, &config.snapshot_cache_dir)
        .map_err(|error| invalid(format!("opening current index snapshot: {error}")))?;
    QueryEngine::production(snapshot, status, config.model_cache_dir.clone())
        .map_err(|error| invalid(format!("opening query engine: {error}")))
}

fn invalid(reason: String) -> Box<SnapshotStatus> {
    Box::new(SnapshotStatus::unusable(SnapshotState::Invalid, reason))
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for X86McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(
            Implementation::new("x86mcp", env!("CARGO_PKG_VERSION"))
                .with_title("x86 Architecture Manuals"),
        )
        .with_instructions(
            "Use x86_index_status before retrieval when freshness matters. Prefer x86_lookup for named architectural entities, x86_search for open-ended evidence, and x86_build_context for bounded prompts. Preserve returned citations and disclose stale or semantic_degraded_reason state.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        resources::list(self).await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(resources::templates())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        resources::read(self, &request.uri).await.map(Into::into)
    }
}

pub async fn serve_stdio(config: AppConfig) -> anyhow::Result<()> {
    let server = X86McpServer::new(config);
    let warm = server.clone();
    // Open the index in the background so the first request does not wait for
    // the snapshot cache to unpack; a missing index is reported, not fatal.
    tokio::spawn(async move {
        if let Err(status) = warm.engine().await {
            tracing::warn!(
                state = ?status.state,
                reasons = %status.reasons.join("; "),
                "serving without a usable index; retrieval fails until `x86mcp setup` installs one"
            );
        }
    });
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("starting MCP stdio service")?;
    service
        .waiting()
        .await
        .context("running MCP stdio service")?;
    Ok(())
}
