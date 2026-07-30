use std::collections::HashSet;

use crate::catalog::{SectionView, TablePage};

use super::{
    EntityKind, EntityState, GetDiagramRequest, GetDiagramResponse, GetOutlineRequest,
    GetOutlineResponse, GetReferencesRequest, GetReferencesResponse, GetSectionRequest,
    GetSectionResponse, GetTableRequest, GetTableResponse, LookupRequest, LookupResponse,
    QueryEngine, QueryError, SearchMode, SearchRequest, SectionNeighbors,
    planner::{next_page_cursor, page_offset},
};

impl QueryEngine {
    pub fn lookup(&self, request: LookupRequest) -> Result<LookupResponse, QueryError> {
        if request.entity.trim().is_empty() {
            return Err(QueryError::InvalidInput(
                "lookup entity cannot be empty".into(),
            ));
        }
        if !(1..=25).contains(&request.limit) {
            return Err(QueryError::InvalidInput(
                "lookup limit must be between 1 and 25".into(),
            ));
        }
        let search = |mode| {
            self.search(SearchRequest {
                query: request.entity.clone(),
                mode,
                vendors: request.vendors.clone(),
                document_ids: Vec::new(),
                kinds: Vec::new(),
                limit: request.limit,
                cursor: None,
            })
        };
        let exact_response = if request.kind == EntityKind::Term {
            None
        } else {
            Some(search(SearchMode::Exact)?)
        };
        let exact_truncated = exact_response.as_ref().is_some_and(|response| {
            response.next_cursor.is_some() || response.candidate_window_truncated
        });
        let exact = exact_response
            .map(|response| response.hits)
            .unwrap_or_default();
        let exact_ids = exact
            .iter()
            .map(|hit| hit.chunk_id.as_str())
            .collect::<HashSet<_>>();
        let related_response = search(SearchMode::Lexical)?;
        let related_truncated =
            related_response.next_cursor.is_some() || related_response.candidate_window_truncated;
        let related = related_response
            .hits
            .into_iter()
            .filter(|hit| !exact_ids.contains(hit.chunk_id.as_str()))
            .collect::<Vec<_>>();
        let entity_state = if exact.is_empty() && related.is_empty() {
            EntityState::NotFound
        } else {
            EntityState::Found
        };
        Ok(LookupResponse {
            state: self.state(None),
            entity_state,
            entity: request.entity,
            kind: request.kind,
            exact,
            related,
            exact_truncated,
            related_truncated,
        })
    }

    pub fn get_section(
        &self,
        request: GetSectionRequest,
    ) -> Result<GetSectionResponse, QueryError> {
        if request.id.trim().is_empty() {
            return Err(QueryError::InvalidInput(
                "section ID cannot be empty".into(),
            ));
        }
        if !(1..=100).contains(&request.block_limit) {
            return Err(QueryError::InvalidInput(
                "section block limit must be between 1 and 100".into(),
            ));
        }
        if let Some(mut section) = self.snapshot.catalog.section(&request.id)? {
            let mut canonical = request.clone();
            let cursor = canonical.cursor.take();
            let (offset, request_hash) = page_offset(
                cursor.as_deref(),
                &self.snapshot.manifest.snapshot_id,
                &canonical,
            )?;
            let start = offset as usize;
            let end = start
                .saturating_add(request.block_limit as usize)
                .min(section.blocks.len());
            let next_cursor = if end < section.blocks.len() {
                Some(next_page_cursor(
                    &self.snapshot.manifest.snapshot_id,
                    &request_hash,
                    end as u32,
                    &section.blocks[end - 1].block_id,
                )?)
            } else {
                None
            };
            section.blocks = section.blocks.get(start..end).unwrap_or(&[]).to_vec();
            let document_id = section
                .blocks
                .first()
                .map(|block| block.document_id.clone())
                .or_else(|| {
                    self.snapshot
                        .catalog
                        .document_id_for_section(&section.section.section_id)
                        .ok()
                        .flatten()
                })
                .ok_or_else(|| QueryError::InvalidInput("section document is missing".into()))?;
            let citation = self.citation_for_section(&document_id, &section.section)?;
            let children = self
                .snapshot
                .catalog
                .outline(&document_id, Some(&section.section.section_id), 1)?
                .into_iter()
                .filter(|node| node.relative_depth == 1)
                .collect();
            let neighbors = if request.include_neighbors {
                section_neighbors(self, &document_id, &section.section.section_id)?
            } else {
                SectionNeighbors::default()
            };
            return Ok(GetSectionResponse {
                state: self.state(None),
                entity_state: EntityState::Found,
                section: Some(section),
                block: None,
                citation: Some(citation),
                children,
                neighbors,
                next_cursor,
            });
        }
        if let Some(block) = self.snapshot.catalog.block(&request.id)? {
            let citation = self.citation_for_block(&block)?;
            return Ok(GetSectionResponse {
                state: self.state(None),
                entity_state: EntityState::Found,
                section: None,
                block: Some(block),
                citation: Some(citation),
                children: Vec::new(),
                neighbors: SectionNeighbors::default(),
                next_cursor: None,
            });
        }
        Ok(GetSectionResponse {
            state: self.state(None),
            entity_state: EntityState::NotFound,
            section: None,
            block: None,
            citation: None,
            children: Vec::new(),
            neighbors: SectionNeighbors::default(),
            next_cursor: None,
        })
    }

    pub fn get_outline(
        &self,
        request: GetOutlineRequest,
    ) -> Result<GetOutlineResponse, QueryError> {
        if request.depth > 8 {
            return Err(QueryError::InvalidInput(
                "outline depth cannot exceed 8".into(),
            ));
        }
        if !(1..=500).contains(&request.limit) {
            return Err(QueryError::InvalidInput(
                "outline limit must be between 1 and 500".into(),
            ));
        }
        let mut canonical = request.clone();
        let cursor = canonical.cursor.take();
        let (offset, request_hash) = page_offset(
            cursor.as_deref(),
            &self.snapshot.manifest.snapshot_id,
            &canonical,
        )?;
        if let Some(document_id) = request.document_id.as_deref() {
            if self.snapshot.catalog.document(document_id)?.is_none() {
                return Ok(GetOutlineResponse {
                    state: self.state(None),
                    entity_state: EntityState::NotFound,
                    documents: Vec::new(),
                    nodes: Vec::new(),
                    next_cursor: None,
                });
            }
            if let Some(root) = request.root_section_id.as_deref()
                && self
                    .snapshot
                    .catalog
                    .document_id_for_section(root)?
                    .as_deref()
                    != Some(document_id)
            {
                return Ok(GetOutlineResponse {
                    state: self.state(None),
                    entity_state: EntityState::NotFound,
                    documents: Vec::new(),
                    nodes: Vec::new(),
                    next_cursor: None,
                });
            }
            let nodes = self.snapshot.catalog.outline(
                document_id,
                request.root_section_id.as_deref(),
                request.depth,
            )?;
            let (nodes, next_cursor) = page(
                nodes,
                offset,
                request.limit,
                &self.snapshot.manifest.snapshot_id,
                &request_hash,
                |node| node.section.section_id.as_str(),
            )?;
            Ok(GetOutlineResponse {
                state: self.state(None),
                entity_state: EntityState::Found,
                documents: Vec::new(),
                nodes,
                next_cursor,
            })
        } else {
            if request.root_section_id.is_some() {
                return Err(QueryError::InvalidInput(
                    "outline root requires a document ID".into(),
                ));
            }
            let documents = self.snapshot.catalog.documents()?;
            let (documents, next_cursor) = page(
                documents,
                offset,
                request.limit,
                &self.snapshot.manifest.snapshot_id,
                &request_hash,
                |document| document.meta.document_id.as_str(),
            )?;
            Ok(GetOutlineResponse {
                state: self.state(None),
                entity_state: EntityState::Found,
                documents,
                nodes: Vec::new(),
                next_cursor,
            })
        }
    }

    pub fn get_table(&self, request: GetTableRequest) -> Result<GetTableResponse, QueryError> {
        if !(1..=100).contains(&request.limit) {
            return Err(QueryError::InvalidInput(
                "table row limit must be between 1 and 100".into(),
            ));
        }
        let table = match request.row_filter.as_deref().map(str::trim) {
            Some(filter) if !filter.is_empty() => {
                self.filtered_table(&request.id, request.offset, request.limit, filter)?
            }
            _ => self
                .snapshot
                .catalog
                .table(&request.id, request.offset, request.limit)?,
        };
        let Some(mut table) = table else {
            return Ok(GetTableResponse {
                state: self.state(None),
                entity_state: EntityState::NotFound,
                table: None,
                citation: None,
            });
        };
        let block = self
            .snapshot
            .catalog
            .block(&table.block_id)?
            .ok_or_else(|| QueryError::InvalidInput("table source block is missing".into()))?;
        let citation = self.citation_for_block(&block)?;
        if !request.include_raw {
            table.raw_source.clear();
        }
        Ok(GetTableResponse {
            state: self.state(None),
            entity_state: EntityState::Found,
            table: Some(table),
            citation: Some(citation),
        })
    }

    pub fn get_diagram(
        &self,
        request: GetDiagramRequest,
    ) -> Result<GetDiagramResponse, QueryError> {
        let Some(mut diagram) = self.snapshot.catalog.diagram(&request.id)? else {
            return Ok(GetDiagramResponse {
                state: self.state(None),
                entity_state: EntityState::NotFound,
                diagram: None,
                citation: None,
                surrounding: Vec::new(),
            });
        };
        let block = self
            .snapshot
            .catalog
            .block(&diagram.source_block_id)?
            .ok_or_else(|| QueryError::InvalidInput("diagram source block is missing".into()))?;
        let citation = self.citation_for_block(&block)?;
        let surrounding = if request.include_surrounding {
            self.snapshot
                .catalog
                .section(&block.section_id)?
                .map(|section| surrounding_blocks(section, &block.block_id))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if !request.include_raw {
            diagram.raw_source.clear();
        }
        Ok(GetDiagramResponse {
            state: self.state(None),
            entity_state: EntityState::Found,
            diagram: Some(diagram),
            citation: Some(citation),
            surrounding,
        })
    }

    pub fn get_references(
        &self,
        request: GetReferencesRequest,
    ) -> Result<GetReferencesResponse, QueryError> {
        if !(1..=200).contains(&request.limit) {
            return Err(QueryError::InvalidInput(
                "reference limit must be between 1 and 200".into(),
            ));
        }
        let mut references = self.snapshot.catalog.references(
            &request.id,
            request.direction,
            request.limit.saturating_add(1),
        )?;
        let has_more = references.len() > request.limit as usize;
        references.truncate(request.limit as usize);
        let found = !references.is_empty() || self.entity_exists(&request.id)?;
        Ok(GetReferencesResponse {
            state: self.state(None),
            entity_state: if found {
                EntityState::Found
            } else {
                EntityState::NotFound
            },
            references,
            has_more,
        })
    }

    fn filtered_table(
        &self,
        id: &str,
        offset: u32,
        limit: u32,
        filter: &str,
    ) -> Result<Option<TablePage>, QueryError> {
        let Some(first) = self.snapshot.catalog.table(id, 0, 200)? else {
            return Ok(None);
        };
        let mut rows = first.rows.clone();
        let mut next_offset = rows.len() as u32;
        while next_offset < first.total_rows {
            let page = self
                .snapshot
                .catalog
                .table(&first.table_id, next_offset, 200)?
                .ok_or_else(|| QueryError::InvalidInput("table disappeared while paging".into()))?;
            next_offset = next_offset.saturating_add(page.rows.len() as u32);
            rows.extend(page.rows);
        }
        let filter = filter.to_lowercase();
        rows.retain(|row| row.iter().any(|cell| cell.to_lowercase().contains(&filter)));
        let total_rows = rows.len() as u32;
        let start = offset as usize;
        let end = start.saturating_add(limit as usize).min(rows.len());
        let selected = rows.get(start..end).unwrap_or(&[]).to_vec();
        Ok(Some(TablePage {
            table_id: first.table_id,
            block_id: first.block_id,
            caption: first.caption,
            headers: first.headers,
            rows: selected,
            total_rows,
            offset,
            limit,
            has_more: end < rows.len(),
            raw_source: first.raw_source,
        }))
    }

    fn entity_exists(&self, id: &str) -> Result<bool, QueryError> {
        if self.snapshot.catalog.document(id)?.is_some()
            || self.snapshot.catalog.section(id)?.is_some()
            || self.snapshot.catalog.block(id)?.is_some()
            || self.snapshot.catalog.table(id, 0, 1)?.is_some()
            || self.snapshot.catalog.diagram(id)?.is_some()
        {
            return Ok(true);
        }
        Ok(false)
    }
}

fn section_neighbors(
    engine: &QueryEngine,
    document_id: &str,
    section_id: &str,
) -> Result<SectionNeighbors, QueryError> {
    let sections = engine.snapshot.catalog.outline(document_id, None, 8)?;
    let Some(index) = sections
        .iter()
        .position(|node| node.section.section_id == section_id)
    else {
        return Ok(SectionNeighbors::default());
    };
    Ok(SectionNeighbors {
        previous: index
            .checked_sub(1)
            .and_then(|previous| sections.get(previous))
            .map(|node| node.section.clone()),
        next: sections.get(index + 1).map(|node| node.section.clone()),
    })
}

fn surrounding_blocks(
    section: SectionView,
    block_id: &str,
) -> Vec<crate::domain::block::SourceBlock> {
    let Some(index) = section
        .blocks
        .iter()
        .position(|block| block.block_id == block_id)
    else {
        return Vec::new();
    };
    let start = index.saturating_sub(1);
    let end = (index + 2).min(section.blocks.len());
    section.blocks[start..end]
        .iter()
        .filter(|block| block.block_id != block_id)
        .cloned()
        .collect()
}

fn page<T>(
    values: Vec<T>,
    offset: u32,
    limit: u32,
    snapshot_id: &str,
    request_hash: &str,
    sort_key: impl Fn(&T) -> &str,
) -> Result<(Vec<T>, Option<String>), QueryError>
where
    T: Clone,
{
    let start = offset as usize;
    let end = start.saturating_add(limit as usize).min(values.len());
    let selected = values.get(start..end).unwrap_or(&[]).to_vec();
    let next = if end < values.len() {
        let key = selected.last().map(&sort_key).unwrap_or_default();
        Some(next_page_cursor(
            snapshot_id,
            request_hash,
            end as u32,
            key,
        )?)
    } else {
        None
    };
    Ok((selected, next))
}
