use rmcp::ErrorData;

use crate::query::{LookupRequest, LookupResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: LookupRequest,
) -> Result<LookupResponse, ErrorData> {
    server.execute(move |engine| engine.lookup(request)).await
}
