use anyhow::Result;

use crate::{config::AppConfig, mcp};

/// Serves MCP over stdio. A missing or unusable index is not fatal: the server
/// starts anyway and reports the problem through `x86_index_status` and errors.
pub async fn run(config: &AppConfig) -> Result<u8> {
    mcp::serve_stdio(config.clone()).await?;
    Ok(0)
}
