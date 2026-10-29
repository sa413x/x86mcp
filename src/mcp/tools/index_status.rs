use rmcp::ErrorData;

use crate::query::IndexStatusResponse;

use super::super::server::X86McpServer;

pub(crate) async fn invoke(server: &X86McpServer) -> Result<IndexStatusResponse, ErrorData> {
    server.index_status().await
}
