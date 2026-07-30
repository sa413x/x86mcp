use rmcp::ErrorData;

use crate::query::{GetTableRequest, GetTableResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: GetTableRequest,
) -> Result<GetTableResponse, ErrorData> {
    server
        .execute(move |engine| engine.get_table(request))
        .await
}
