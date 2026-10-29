use rmcp::ErrorData;

use crate::{domain::Vendor, query::QueryError};

pub(crate) fn query_error(error: QueryError) -> ErrorData {
    match error {
        QueryError::InvalidInput(message) => ErrorData::invalid_params(message, None),
        other => ErrorData::internal_error(other.to_string(), None),
    }
}

pub(crate) fn join_error(error: tokio::task::JoinError) -> ErrorData {
    ErrorData::internal_error(format!("query worker failed: {error}"), None)
}

/// Vendor name as written in prose.
pub(crate) fn vendor_label(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Intel => "Intel",
        Vendor::Amd => "AMD",
    }
}
