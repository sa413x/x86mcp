use rmcp::ErrorData;

use crate::query::QueryError;

pub(crate) fn query_error(error: QueryError) -> ErrorData {
    match error {
        QueryError::InvalidInput(message) => ErrorData::invalid_params(message, None),
        other => ErrorData::internal_error(other.to_string(), None),
    }
}

pub(crate) fn join_error(error: tokio::task::JoinError) -> ErrorData {
    ErrorData::internal_error(format!("query worker failed: {error}"), None)
}
