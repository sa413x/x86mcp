use std::sync::Arc;

use anyhow::Context;
use rmcp::{ErrorData, ServiceExt};

use crate::query::{QueryEngine, QueryError};

use super::types::{join_error, query_error};

#[derive(Clone)]
pub struct X86McpServer {
    pub(crate) engine: Arc<QueryEngine>,
}

impl X86McpServer {
    pub fn new(engine: Arc<QueryEngine>) -> Self {
        Self { engine }
    }

    pub(crate) async fn execute<T, F>(&self, operation: F) -> Result<T, ErrorData>
    where
        T: Send + 'static,
        F: FnOnce(&QueryEngine) -> Result<T, QueryError> + Send + 'static,
    {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || operation(&engine))
            .await
            .map_err(join_error)?
            .map_err(query_error)
    }
}

pub async fn serve_stdio(engine: Arc<QueryEngine>) -> anyhow::Result<()> {
    let service = X86McpServer::new(engine)
        .serve(rmcp::transport::stdio())
        .await
        .context("starting MCP stdio service")?;
    service
        .waiting()
        .await
        .context("running MCP stdio service")?;
    Ok(())
}
