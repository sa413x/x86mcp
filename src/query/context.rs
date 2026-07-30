use std::collections::{HashMap, HashSet};

use crate::domain::chunk::SearchChunk;

use super::{
    BuildContextRequest, BuildContextResponse, ContextItem, ContextOmission, QueryEngine,
    QueryError, SearchRequest,
};

impl QueryEngine {
    pub fn build_context(
        &self,
        request: BuildContextRequest,
    ) -> Result<BuildContextResponse, QueryError> {
        if !(256..=32_768).contains(&request.token_budget) {
            return Err(QueryError::InvalidInput(
                "context token budget must be between 256 and 32768".into(),
            ));
        }
        if request.chunk_ids.len() > 256 {
            return Err(QueryError::InvalidInput(
                "at most 256 explicit chunk IDs are accepted".into(),
            ));
        }
        if request
            .query
            .as_deref()
            .is_none_or(|query| query.trim().is_empty())
            && request.chunk_ids.is_empty()
        {
            return Err(QueryError::InvalidInput(
                "context requires a query or chunk IDs".into(),
            ));
        }

        let mut ordered_ids = Vec::new();
        let mut seen = HashSet::new();
        let mut degraded_reason = None;
        if let Some(query) = request.query.as_deref()
            && !query.trim().is_empty()
        {
            let response = self.search(SearchRequest {
                query: query.to_owned(),
                mode: request.mode,
                vendors: request.vendors.clone(),
                document_ids: Vec::new(),
                kinds: Vec::new(),
                limit: 50,
                cursor: None,
            })?;
            degraded_reason = response.state.semantic_degraded_reason;
            for hit in response.hits {
                if seen.insert(hit.chunk_id.clone()) {
                    ordered_ids.push(hit.chunk_id);
                }
            }
        }
        for chunk_id in request.chunk_ids {
            if seen.insert(chunk_id.clone()) {
                ordered_ids.push(chunk_id);
            }
        }

        let chunks = self
            .snapshot
            .catalog
            .chunks_by_ids(&ordered_ids)?
            .into_iter()
            .map(|chunk| (chunk.chunk_id.clone(), chunk))
            .collect::<HashMap<_, _>>();
        let mut omissions = ordered_ids
            .iter()
            .filter(|chunk_id| !chunks.contains_key(*chunk_id))
            .map(|chunk_id| ContextOmission {
                chunk_id: chunk_id.clone(),
                reason: "chunk ID was not found in the current snapshot".into(),
            })
            .collect::<Vec<_>>();
        let groups = adjacent_groups(
            ordered_ids
                .iter()
                .filter_map(|chunk_id| chunks.get(chunk_id))
                .collect(),
            |chunk_id| self.metadata(chunk_id).map(|metadata| metadata.vector_row),
        );

        let mut items = Vec::new();
        let mut estimated_tokens = 0_u32;
        for group in groups {
            let text = merge_group_text(&group);
            let estimate = estimate_tokens(&text);
            if estimated_tokens.saturating_add(estimate) > request.token_budget {
                omissions.extend(group.iter().map(|chunk| ContextOmission {
                    chunk_id: chunk.chunk_id.clone(),
                    reason: "context token budget exhausted".into(),
                }));
                continue;
            }
            let citations = group
                .iter()
                .map(|chunk| self.citation_for_chunk(chunk))
                .collect::<Result<Vec<_>, _>>()?;
            estimated_tokens += estimate;
            items.push(ContextItem {
                text,
                chunk_ids: group.iter().map(|chunk| chunk.chunk_id.clone()).collect(),
                citations,
                estimated_tokens: estimate,
            });
        }
        Ok(BuildContextResponse {
            state: self.state(degraded_reason),
            items,
            omitted: omissions,
            estimated_tokens,
        })
    }
}

fn adjacent_groups(
    chunks: Vec<&SearchChunk>,
    row: impl Fn(&str) -> Option<u64>,
) -> Vec<Vec<&SearchChunk>> {
    let mut groups = Vec::<Vec<&SearchChunk>>::new();
    for chunk in chunks {
        let adjacent = groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|previous| {
                previous.document_id == chunk.document_id
                    && previous.section_id == chunk.section_id
                    && row(&previous.chunk_id)
                        .zip(row(&chunk.chunk_id))
                        .is_some_and(|(left, right)| left.checked_add(1) == Some(right))
            });
        if adjacent {
            groups.last_mut().expect("one context group").push(chunk);
        } else {
            groups.push(vec![chunk]);
        }
    }
    groups
}

fn merge_group_text(chunks: &[&SearchChunk]) -> String {
    let mut text = String::new();
    for chunk in chunks {
        append_without_line_overlap(&mut text, &chunk.text);
    }
    text
}

fn append_without_line_overlap(output: &mut String, next: &str) {
    if output.is_empty() {
        output.push_str(next);
        return;
    }
    let left = output.lines().collect::<Vec<_>>();
    let right = next.lines().collect::<Vec<_>>();
    let maximum = left.len().min(right.len());
    let overlap = (1..=maximum)
        .rev()
        .find(|&count| left[left.len() - count..] == right[..count])
        .unwrap_or(0);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    if overlap == 0 {
        output.push('\n');
    }
    output.push_str(&right[overlap..].join("\n"));
}

pub(crate) fn estimate_tokens(text: &str) -> u32 {
    let characters = text.chars().count() as u64;
    ((characters.saturating_mul(2).saturating_add(6)) / 7)
        .try_into()
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_conservative_and_rounded_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens(&"x".repeat(35)), 10);
    }

    #[test]
    fn line_overlap_is_not_repeated() {
        let mut text = "heading\nshared".to_owned();
        append_without_line_overlap(&mut text, "shared\nnext");
        assert_eq!(text, "heading\nshared\nnext");
    }
}
