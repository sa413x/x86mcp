use rmcp::ErrorData;

use crate::query::{GetDiagramRequest, GetDiagramResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: GetDiagramRequest,
) -> Result<GetDiagramResponse, ErrorData> {
    server
        .execute(move |engine| engine.get_diagram(request))
        .await
}
