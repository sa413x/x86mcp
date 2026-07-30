use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::index::LexicalSearchRequest;

use super::{QueryError, SearchRequest};

const MAX_CURSOR_OFFSET: u32 = 10_000;
const MAX_FILTER_VALUES: usize = 16;
const MAX_SEARCH_CANDIDATES: u32 = 300;

pub(crate) struct SearchPlan {
    pub lexical: LexicalSearchRequest,
    pub request_hash: String,
    pub offset: u32,
    pub previous_sort_key: Option<String>,
    pub fetch_limit: u32,
}

#[derive(Deserialize, Serialize)]
struct CursorPayload {
    snapshot_id: String,
    request_hash: String,
    offset: u32,
    sort_key: String,
}

pub(crate) fn plan_search(
    request: &SearchRequest,
    snapshot_id: &str,
) -> Result<SearchPlan, QueryError> {
    if request.query.trim().is_empty() {
        return Err(QueryError::InvalidInput("query cannot be empty".into()));
    }
    if !(1..=50).contains(&request.limit) {
        return Err(QueryError::InvalidInput(
            "search limit must be between 1 and 50".into(),
        ));
    }
    validate_filters(request)?;
    let request_hash = request_hash(request)?;
    let decoded_cursor = request
        .cursor
        .as_deref()
        .map(|cursor| decode_cursor(cursor, snapshot_id, &request_hash))
        .transpose()?;
    let offset = decoded_cursor.as_ref().map_or(0, |cursor| cursor.offset);
    if offset > MAX_CURSOR_OFFSET || offset >= MAX_SEARCH_CANDIDATES {
        return Err(QueryError::InvalidInput(
            "search cursor is beyond the retained candidate window".into(),
        ));
    }
    let fetch_limit = 100;
    let lexical = LexicalSearchRequest::from_query(&request.query, fetch_limit)?;
    Ok(SearchPlan {
        lexical,
        request_hash,
        offset,
        previous_sort_key: decoded_cursor.map(|cursor| cursor.sort_key),
        fetch_limit,
    })
}

pub(crate) fn next_search_cursor(
    snapshot_id: &str,
    request_hash: &str,
    offset: u32,
    sort_key: &str,
) -> Result<String, QueryError> {
    encode_cursor(&CursorPayload {
        snapshot_id: snapshot_id.to_owned(),
        request_hash: request_hash.to_owned(),
        offset,
        sort_key: sort_key.to_owned(),
    })
}

pub(crate) fn page_offset<T: Serialize>(
    cursor: Option<&str>,
    snapshot_id: &str,
    request_without_cursor: &T,
) -> Result<(u32, String), QueryError> {
    let canonical = serde_json::to_vec(request_without_cursor)
        .map_err(|error| QueryError::InvalidInput(error.to_string()))?;
    let request_hash = blake3::hash(&canonical).to_hex().to_string();
    let offset = match cursor {
        Some(cursor) => decode_cursor(cursor, snapshot_id, &request_hash)?.offset,
        None => 0,
    };
    if offset > MAX_CURSOR_OFFSET {
        return Err(QueryError::InvalidInput(
            "cursor offset exceeds the supported range".into(),
        ));
    }
    Ok((offset, request_hash))
}

pub(crate) fn next_page_cursor(
    snapshot_id: &str,
    request_hash: &str,
    offset: u32,
    sort_key: &str,
) -> Result<String, QueryError> {
    next_search_cursor(snapshot_id, request_hash, offset, sort_key)
}

fn validate_filters(request: &SearchRequest) -> Result<(), QueryError> {
    if request.vendors.len() > 2
        || request.document_ids.len() > MAX_FILTER_VALUES
        || request.kinds.len() > MAX_FILTER_VALUES
    {
        return Err(QueryError::InvalidInput(
            "too many search filter values".into(),
        ));
    }
    let combinations = request.vendors.len().max(1)
        * request.document_ids.len().max(1)
        * request.kinds.len().max(1);
    if combinations > 64 {
        return Err(QueryError::InvalidInput(
            "search filters produce more than 64 combinations".into(),
        ));
    }
    if request
        .document_ids
        .iter()
        .any(|document| document.trim().is_empty())
    {
        return Err(QueryError::InvalidInput(
            "document filters cannot be empty".into(),
        ));
    }
    if has_duplicates(&request.vendors)
        || has_duplicates(&request.document_ids)
        || has_duplicates(&request.kinds)
    {
        return Err(QueryError::InvalidInput(
            "search filters cannot contain duplicates".into(),
        ));
    }
    Ok(())
}

fn has_duplicates<T: Eq>(values: &[T]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}

fn request_hash(request: &SearchRequest) -> Result<String, QueryError> {
    let mut canonical = request.clone();
    canonical.cursor = None;
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| QueryError::InvalidInput(error.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn encode_cursor(payload: &CursorPayload) -> Result<String, QueryError> {
    let bytes =
        serde_json::to_vec(payload).map_err(|error| QueryError::InvalidInput(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(
    cursor: &str,
    snapshot_id: &str,
    request_hash: &str,
) -> Result<CursorPayload, QueryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| QueryError::InvalidInput("cursor is not valid URL-safe base64".into()))?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| QueryError::InvalidInput("cursor payload is invalid".into()))?;
    if payload.snapshot_id != snapshot_id {
        return Err(QueryError::InvalidInput(
            "cursor belongs to a different snapshot".into(),
        ));
    }
    if payload.request_hash != request_hash {
        return Err(QueryError::InvalidInput(
            "cursor does not match the request".into(),
        ));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use crate::{domain::Vendor, domain::chunk::ChunkKind, query::SearchMode};

    use super::*;

    fn request() -> SearchRequest {
        SearchRequest {
            query: "CR4.VMXE".into(),
            mode: SearchMode::Hybrid,
            vendors: vec![Vendor::Intel],
            document_ids: Vec::new(),
            kinds: vec![ChunkKind::Prose],
            limit: 10,
            cursor: None,
        }
    }

    #[test]
    fn cursor_rejects_request_and_snapshot_mismatch() {
        let request = request();
        let plan = plan_search(&request, "snapshot-a").unwrap();
        let cursor = next_search_cursor("snapshot-a", &plan.request_hash, 10, "chk:last").unwrap();
        let mut changed = request.clone();
        changed.limit = 5;
        changed.cursor = Some(cursor.clone());
        assert!(matches!(
            plan_search(&changed, "snapshot-a"),
            Err(QueryError::InvalidInput(_))
        ));
        let mut stale = request;
        stale.cursor = Some(cursor);
        assert!(matches!(
            plan_search(&stale, "snapshot-b"),
            Err(QueryError::InvalidInput(_))
        ));
    }
}
