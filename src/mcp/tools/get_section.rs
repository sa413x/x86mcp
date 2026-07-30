use rmcp::ErrorData;

use crate::query::{GetSectionRequest, GetSectionResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: GetSectionRequest,
) -> Result<GetSectionResponse, ErrorData> {
    server
        .execute(move |engine| engine.get_section(request))
        .await
}
