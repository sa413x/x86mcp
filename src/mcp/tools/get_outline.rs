use rmcp::ErrorData;

use crate::query::{GetOutlineRequest, GetOutlineResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: GetOutlineRequest,
) -> Result<GetOutlineResponse, ErrorData> {
    server
        .execute(move |engine| engine.get_outline(request))
        .await
}
