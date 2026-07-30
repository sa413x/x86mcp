use rmcp::ErrorData;

use crate::query::{SearchRequest, SearchResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: SearchRequest,
) -> Result<SearchResponse, ErrorData> {
    server.execute(move |engine| engine.search(request)).await
}
