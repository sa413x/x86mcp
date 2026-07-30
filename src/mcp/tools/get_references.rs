use rmcp::ErrorData;

use crate::query::{GetReferencesRequest, GetReferencesResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: GetReferencesRequest,
) -> Result<GetReferencesResponse, ErrorData> {
    server
        .execute(move |engine| engine.get_references(request))
        .await
}
