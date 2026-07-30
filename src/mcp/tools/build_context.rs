use rmcp::ErrorData;

use crate::query::{BuildContextRequest, BuildContextResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: BuildContextRequest,
) -> Result<BuildContextResponse, ErrorData> {
    server
        .execute(move |engine| engine.build_context(request))
        .await
}
