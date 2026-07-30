use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::{
    domain::{Vendor, chunk::SearchChunk},
    index::LexicalSearchRequest,
};

use super::{
    CompareVendorsRequest, CompareVendorsResponse, QueryEngine, QueryError, SearchHit, SearchMode,
    SearchRequest, SearchResponse,
    planner::{next_search_cursor, plan_search},
    ranking::{ScoredId, fuse},
};

const CATALOG_CHUNK_BATCH_SIZE: usize = 256;

impl QueryEngine {
    pub fn search(&self, request: SearchRequest) -> Result<SearchResponse, QueryError> {
        let plan = plan_search(&request, &self.snapshot.manifest.snapshot_id)?;
        let needs_exact = matches!(request.mode, SearchMode::Exact | SearchMode::Hybrid);
        let needs_lexical = matches!(request.mode, SearchMode::Lexical | SearchMode::Hybrid);
        let needs_semantic = matches!(request.mode, SearchMode::Semantic | SearchMode::Hybrid);

        let exact = if needs_exact {
            self.lexical_candidates(&request, &plan.lexical, true)?
        } else {
            Vec::new()
        };
        let lexical = if needs_lexical {
            self.lexical_candidates(&request, &plan.lexical, false)?
        } else {
            Vec::new()
        };
        let (semantic, semantic_degraded_reason) = if needs_semantic {
            match self.semantic_candidates(&request, plan.fetch_limit) {
                Ok(candidates) => (candidates, self.base_semantic_degraded_reason()),
                Err(reason) => (Vec::new(), Some(reason)),
            }
        } else {
            (Vec::new(), None)
        };
        let candidate_window_truncated = exact.len() >= plan.fetch_limit as usize
            || lexical.len() >= plan.fetch_limit as usize
            || semantic.len() >= plan.fetch_limit as usize;

        let candidate_ids = exact
            .iter()
            .chain(&lexical)
            .chain(&semantic)
            .map(|candidate| candidate.chunk_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut chunks = BTreeMap::new();
        for batch in candidate_ids.chunks(CATALOG_CHUNK_BATCH_SIZE) {
            for chunk in self.snapshot.catalog.chunks_by_ids(batch)? {
                chunks.insert(chunk.chunk_id.clone(), chunk);
            }
        }
        let exact_symbols = plan
            .lexical
            .exact_symbols
            .iter()
            .map(|symbol| crate::index::normalize_symbol(symbol))
            .collect::<HashSet<_>>();
        let mut ranked = fuse(&exact, &lexical, &semantic, |chunk_id| {
            chunks
                .get(chunk_id)
                .map(|chunk| boost(chunk, &request, &exact_symbols))
                .unwrap_or(0.0)
        });
        let mut per_section = HashMap::<String, usize>::new();
        ranked.retain(|candidate| {
            let Some(chunk) = chunks.get(&candidate.chunk_id) else {
                return false;
            };
            let count = per_section.entry(chunk.section_id.clone()).or_default();
            if *count >= 2 {
                return false;
            }
            *count += 1;
            true
        });
        if let Some(previous_sort_key) = plan.previous_sort_key.as_deref()
            && (plan.offset == 0
                || ranked
                    .get(plan.offset as usize - 1)
                    .is_none_or(|candidate| candidate.chunk_id != previous_sort_key))
        {
            return Err(QueryError::InvalidInput(
                "cursor sort key does not match the current ranking".into(),
            ));
        }

        let start = plan.offset as usize;
        let end = start
            .saturating_add(request.limit as usize)
            .min(ranked.len());
        let selected = ranked.get(start..end).unwrap_or(&[]);
        let mut hits = Vec::with_capacity(selected.len());
        for candidate in selected {
            let chunk = chunks.get(&candidate.chunk_id).ok_or_else(|| {
                QueryError::InvalidInput(format!(
                    "ranked chunk {} is absent from catalog",
                    candidate.chunk_id
                ))
            })?;
            hits.push(SearchHit {
                chunk_id: chunk.chunk_id.clone(),
                snippet: Self::snippet(&chunk.text),
                citation: self.citation_for_chunk(chunk)?,
                scores: candidate.scores.clone(),
            });
        }
        let next_cursor = if end < ranked.len() {
            Some(next_search_cursor(
                &self.snapshot.manifest.snapshot_id,
                &plan.request_hash,
                end as u32,
                &ranked[end - 1].chunk_id,
            )?)
        } else {
            None
        };
        Ok(SearchResponse {
            state: self.state(semantic_degraded_reason),
            hits,
            next_cursor,
            candidate_window_truncated,
        })
    }

    pub fn compare_vendors(
        &self,
        request: CompareVendorsRequest,
    ) -> Result<CompareVendorsResponse, QueryError> {
        if !(1..=25).contains(&request.limit_per_vendor) {
            return Err(QueryError::InvalidInput(
                "vendor comparison limit must be between 1 and 25".into(),
            ));
        }
        let search_for = |vendor| {
            self.search(SearchRequest {
                query: request.query.clone(),
                mode: request.mode,
                vendors: vec![vendor],
                document_ids: Vec::new(),
                kinds: Vec::new(),
                limit: request.limit_per_vendor,
                cursor: None,
            })
        };
        let intel = search_for(Vendor::Intel)?;
        let amd = search_for(Vendor::Amd)?;
        let intel_truncated = intel.next_cursor.is_some() || intel.candidate_window_truncated;
        let amd_truncated = amd.next_cursor.is_some() || amd.candidate_window_truncated;
        let semantic_degraded_reason = intel
            .state
            .semantic_degraded_reason
            .clone()
            .or(amd.state.semantic_degraded_reason);
        Ok(CompareVendorsResponse {
            state: self.state(semantic_degraded_reason),
            intel: intel.hits,
            amd: amd.hits,
            intel_truncated,
            amd_truncated,
        })
    }

    fn lexical_candidates(
        &self,
        request: &SearchRequest,
        base: &LexicalSearchRequest,
        exact_only: bool,
    ) -> Result<Vec<ScoredId>, QueryError> {
        if exact_only && base.exact_symbols.is_empty() {
            return Ok(Vec::new());
        }
        let vendors = option_values(&request.vendors);
        let documents = option_values(&request.document_ids);
        let kinds = option_values(&request.kinds);
        let mut candidates = HashMap::<String, f32>::new();
        for vendor in &vendors {
            for document in &documents {
                for kind in &kinds {
                    let lexical = LexicalSearchRequest {
                        words: if exact_only {
                            Vec::new()
                        } else {
                            base.words.clone()
                        },
                        exact_symbols: base.exact_symbols.clone(),
                        vendor: vendor.copied(),
                        document_id: document.map(|value| value.to_owned()),
                        kind: kind.copied(),
                        limit: base.limit,
                    };
                    for hit in self.snapshot.lexical.search(&lexical)? {
                        candidates
                            .entry(hit.chunk_id)
                            .and_modify(|score| *score = score.max(hit.score))
                            .or_insert(hit.score);
                    }
                }
            }
        }
        let mut candidates = candidates
            .into_iter()
            .map(|(chunk_id, score)| ScoredId { chunk_id, score })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        candidates.truncate(base.limit as usize);
        Ok(candidates)
    }

    fn semantic_candidates(
        &self,
        request: &SearchRequest,
        fetch_limit: u32,
    ) -> Result<Vec<ScoredId>, String> {
        let embedder = self.semantic_embedder()?;
        let query = embedder
            .embed_query(&request.query)
            .map_err(|error| error.to_string())?;
        let allowed_rows = self
            .vector_metadata
            .iter()
            .filter(|metadata| metadata_matches(metadata, request))
            .map(|metadata| metadata.vector_row)
            .collect::<Vec<_>>();
        if allowed_rows.is_empty() {
            return Ok(Vec::new());
        }
        let limit = (fetch_limit as usize).min(allowed_rows.len());
        let hits = self
            .snapshot
            .vectors
            .top_k(&query, Some(&allowed_rows), limit)
            .map_err(|error| error.to_string())?;
        Ok(hits
            .into_iter()
            .map(|hit| ScoredId {
                chunk_id: self.vector_metadata[hit.row as usize].chunk_id.clone(),
                score: hit.score,
            })
            .collect())
    }
}

fn option_values<T>(values: &[T]) -> Vec<Option<&T>> {
    if values.is_empty() {
        vec![None]
    } else {
        values.iter().map(Some).collect()
    }
}

fn metadata_matches(
    metadata: &crate::catalog::VectorChunkMetadata,
    request: &SearchRequest,
) -> bool {
    (request.vendors.is_empty() || request.vendors.contains(&metadata.vendor))
        && (request.document_ids.is_empty() || request.document_ids.contains(&metadata.document_id))
        && (request.kinds.is_empty() || request.kinds.contains(&metadata.kind))
}

fn boost(chunk: &SearchChunk, request: &SearchRequest, exact_symbols: &HashSet<String>) -> f32 {
    let exact = chunk
        .symbols
        .iter()
        .any(|symbol| exact_symbols.contains(&crate::index::normalize_symbol(symbol)));
    let heading = chunk
        .text
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let query = request.query.to_lowercase();
    let mut value = if exact { 3.0 } else { 1.0 };
    if heading.contains(&query)
        || exact_symbols
            .iter()
            .any(|symbol| heading.contains(&symbol.to_lowercase()))
    {
        value *= 1.25;
    }
    if chunk.content_class != crate::domain::block::ContentClass::Substantive {
        value *= 0.35;
    }
    if !request.vendors.is_empty() {
        value *= 1.03;
    }
    if !request.document_ids.is_empty() {
        value *= 1.03;
    }
    if !request.kinds.is_empty() {
        value *= 1.03;
    }
    value
}
