use rmcp::ErrorData;

use crate::query::{CompareVendorsRequest, CompareVendorsResponse};

use super::super::server::X86McpServer;

pub(crate) async fn invoke(
    server: &X86McpServer,
    request: CompareVendorsRequest,
) -> Result<CompareVendorsResponse, ErrorData> {
    server
        .execute(move |engine| engine.compare_vendors(request))
        .await
}
