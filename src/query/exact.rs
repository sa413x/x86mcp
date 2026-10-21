use std::collections::{HashMap, HashSet};

use crate::{
    catalog::{SectionView, TablePage},
    domain::chunk::SearchChunk,
};

use super::{
    EntityKind, EntityState, GetDiagramRequest, GetDiagramResponse, GetOutlineRequest,
    GetOutlineResponse, GetReferencesRequest, GetReferencesResponse, GetSectionRequest,
    GetSectionResponse, GetTableRequest, GetTableResponse, LookupRequest, LookupResponse,
    QueryEngine, QueryError, SearchHit, SearchMode, SearchRequest, SectionNeighbors,
    planner::{next_page_cursor, page_offset},
};

/// Page size used while scanning exact-symbol candidates for kind-qualified evidence.
const EXACT_SCAN_PAGE: u32 = 50;

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
        let limit = request.limit as usize;
        let search = |mode, limit, cursor| {
            self.search(SearchRequest {
                query: request.entity.clone(),
                mode,
                vendors: request.vendors.clone(),
                document_ids: Vec::new(),
                kinds: Vec::new(),
                limit,
                cursor,
            })
        };
        // Exact-symbol hits only count as exact evidence when the chunk carries a
        // signal for the requested kind; the rest are demoted to related evidence.
        let mut exact = Vec::new();
        let mut mismatched = Vec::new();
        let mut exact_truncated = false;
        if request.kind != EntityKind::Term {
            let mut cursor = None;
            loop {
                let response = search(SearchMode::Exact, EXACT_SCAN_PAGE, cursor)?;
                let ids = response
                    .hits
                    .iter()
                    .map(|hit| hit.chunk_id.clone())
                    .collect::<Vec<_>>();
                let chunks = self
                    .snapshot
                    .catalog
                    .chunks_by_ids(&ids)?
                    .into_iter()
                    .map(|chunk| (chunk.chunk_id.clone(), chunk))
                    .collect::<HashMap<_, _>>();
                for hit in response.hits {
                    let qualified = chunks
                        .get(&hit.chunk_id)
                        .is_some_and(|chunk| kind_matches(request.kind, &request.entity, chunk));
                    if !qualified {
                        mismatched.push(hit);
                    } else if exact.len() < limit {
                        exact.push(hit);
                    } else {
                        exact_truncated = true;
                    }
                }
                match response.next_cursor {
                    Some(next) if !exact_truncated => cursor = Some(next),
                    _ => {
                        exact_truncated |= response.candidate_window_truncated;
                        break;
                    }
                }
            }
        }
        let related_response = search(SearchMode::Lexical, request.limit, None)?;
        let mut related_truncated =
            related_response.next_cursor.is_some() || related_response.candidate_window_truncated;
        let mut seen = exact
            .iter()
            .chain(&mismatched)
            .map(|hit| hit.chunk_id.clone())
            .collect::<HashSet<_>>();
        let mut related: Vec<SearchHit> = mismatched;
        related.extend(
            related_response
                .hits
                .into_iter()
                .filter(|hit| seen.insert(hit.chunk_id.clone())),
        );
        if related.len() > limit {
            related.truncate(limit);
            related_truncated = true;
        }
        let found = if request.kind == EntityKind::Term {
            !related.is_empty()
        } else {
            !exact.is_empty()
        };
        let entity_state = if found {
            EntityState::Found
        } else {
            EntityState::NotFound
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
            warnings: first.warnings,
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

/// Decides whether an exact-symbol hit is evidence for `entity` as the requested `kind`.
///
/// Every typed kind first requires the chunk to name the entity. Kinds whose names follow
/// architectural conventions (`IA32_*` MSRs, `#GP` exceptions, `CR4` registers, `CR4.VMXE`
/// bitfields, `CPUID.01H` leaves) qualify by that shape; other names qualify only when a
/// heading, line, or sentence pairs the entity with a kind marker. Instructions require an
/// instruction-reference heading that begins with the mnemonic (`MOV—Move`, `VMRUN Run
/// Virtual Machine`).
fn kind_matches(kind: EntityKind, entity: &str, chunk: &SearchChunk) -> bool {
    let entity = entity.trim();
    if kind == EntityKind::Term {
        return true;
    }
    let segments = evidence_segments(chunk);
    let normalized = crate::index::normalize_symbol(entity);
    let mentioned = segments
        .iter()
        .any(|segment| contains_token(segment, entity))
        || chunk
            .symbols
            .iter()
            .any(|symbol| crate::index::normalize_symbol(symbol) == normalized);
    if !mentioned {
        return false;
    }
    match kind {
        EntityKind::Term => true,
        EntityKind::Instruction => {
            is_mnemonic(entity)
                && chunk
                    .heading_path
                    .iter()
                    .any(|heading| heading_names_instruction(heading, entity))
        }
        EntityKind::Msr => {
            is_msr_name(entity)
                || co_occurs(
                    &segments,
                    entity,
                    &[
                        "MSR",
                        "MSRs",
                        "model-specific register",
                        "model specific register",
                    ],
                )
        }
        EntityKind::Cpuid => {
            if is_cpuid_reference(entity) {
                segments
                    .iter()
                    .any(|segment| contains_token(segment, "CPUID"))
            } else {
                co_occurs(&segments, entity, &["CPUID"])
            }
        }
        EntityKind::Register => {
            is_register_name(entity) || co_occurs(&segments, entity, &["register", "registers"])
        }
        EntityKind::Exception => {
            is_exception_mnemonic(entity)
                || co_occurs(
                    &segments,
                    entity,
                    &[
                        "exception",
                        "exceptions",
                        "fault",
                        "trap",
                        "abort",
                        "interrupt",
                    ],
                )
        }
        EntityKind::Bitfield => {
            is_bitfield_reference(entity)
                || co_occurs(
                    &segments,
                    entity,
                    &["bit", "bits", "flag", "flags", "field", "fields"],
                )
        }
    }
}

/// Headings plus the lines and sentences of the chunk text.
fn evidence_segments(chunk: &SearchChunk) -> Vec<&str> {
    chunk
        .heading_path
        .iter()
        .map(String::as_str)
        .chain(chunk.text.lines().flat_map(|line| line.split(". ")))
        .filter(|segment| !segment.trim().is_empty())
        .collect()
}

fn co_occurs(segments: &[&str], entity: &str, markers: &[&str]) -> bool {
    segments.iter().any(|segment| {
        contains_token(segment, entity)
            && markers.iter().any(|marker| contains_token(segment, marker))
    })
}

/// ASCII case-insensitive search for `needle` delimited by non-identifier characters.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    let (Some(&first), Some(&last)) = (needle.first(), needle.last()) else {
        return false;
    };
    if needle.len() > haystack.len() {
        return false;
    }
    (0..=haystack.len() - needle.len()).any(|start| {
        let end = start + needle.len();
        haystack[start..end].eq_ignore_ascii_case(needle)
            && (!is_identifier_byte(first)
                || start == 0
                || !is_identifier_byte(haystack[start - 1]))
            && (!is_identifier_byte(last)
                || haystack
                    .get(end)
                    .is_none_or(|&byte| !is_identifier_byte(byte)))
    })
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// One or more `/`-separated mnemonics such as `CVTSI2SD`, `Jcc`, or `MOVS/MOVSB`.
fn is_mnemonic(entity: &str) -> bool {
    entity.split('/').all(|part| {
        part.len() <= 24
            && part.starts_with(|character: char| character.is_ascii_alphabetic())
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && !is_register_name(part)
    })
}

/// True for unnumbered instruction-reference headings whose leading mnemonic list
/// (`MOVSX/MOVSXD—Move With Sign-Extension`, `VMRUN Run Virtual Machine`) names `entity`.
fn heading_names_instruction(heading: &str, entity: &str) -> bool {
    let heading = heading.trim();
    if heading.starts_with(|character: char| character.is_ascii_digit()) {
        return false;
    }
    let lead = heading
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '—' | '–' | '-' | '(' | ',' | ':')
        })
        .next()
        .unwrap_or_default();
    let mnemonics = lead.split('/').collect::<Vec<_>>();
    mnemonics
        .iter()
        .all(|mnemonic| is_reference_mnemonic(mnemonic))
        && entity.split('/').all(|part| {
            mnemonics
                .iter()
                .any(|mnemonic| mnemonic.eq_ignore_ascii_case(part))
        })
}

/// Manual spelling of a mnemonic: upper-case, optionally ending in a short lower-case
/// operand placeholder (`CMOVcc`, `Jcc`, `PREFETCHh`).
fn is_reference_mnemonic(mnemonic: &str) -> bool {
    let trimmed = mnemonic.trim_end_matches(|character: char| character.is_ascii_lowercase());
    mnemonic.len() - trimmed.len() <= 2
        && trimmed.starts_with(|character: char| character.is_ascii_uppercase())
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_msr_name(entity: &str) -> bool {
    let upper = entity.to_ascii_uppercase();
    upper.starts_with("IA32_") || upper.starts_with("MSR_") || is_hex_address(&upper)
}

/// Hexadecimal MSR addresses or CPUID leaves: `C000_0080h`, `0xC0000080`, `174H`.
fn is_hex_address(upper: &str) -> bool {
    let (digits, marked) = if let Some(digits) = upper.strip_prefix("0X") {
        (digits, true)
    } else if let Some(digits) = upper.strip_suffix('H') {
        let marked = digits.contains('_') || digits.starts_with(|c: char| c.is_ascii_digit());
        (digits, marked)
    } else {
        (upper, upper.contains('_'))
    };
    let hex = digits.bytes().filter(|byte| *byte != b'_').count();
    marked
        && (1..=8).contains(&hex)
        && !digits.starts_with('_')
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'_')
}

fn is_cpuid_reference(entity: &str) -> bool {
    let upper = entity.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix("CPUID") {
        return !rest.starts_with(|character: char| character.is_ascii_alphanumeric());
    }
    if let Some(rest) = upper.strip_prefix("FN") {
        return rest.starts_with(|character: char| character.is_ascii_hexdigit())
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    }
    is_hex_address(&upper)
}

fn is_register_name(entity: &str) -> bool {
    const NAMED: &[&str] = &[
        "RFLAGS", "EFLAGS", "FLAGS", "RIP", "EIP", "IP", "EFER", "MXCSR", "GDTR", "IDTR", "LDTR",
        "TR", "CS", "DS", "ES", "FS", "GS", "SS", "PKRU", "TPR", "SSP", "RAX", "EAX", "AX", "AL",
        "AH", "RBX", "EBX", "BX", "BL", "BH", "RCX", "ECX", "CX", "CL", "CH", "RDX", "EDX", "DX",
        "DL", "DH", "RSI", "ESI", "SI", "SIL", "RDI", "EDI", "DI", "DIL", "RSP", "ESP", "SP",
        "SPL", "RBP", "EBP", "BP", "BPL",
    ];
    const NUMBERED: &[(&str, &[&str])] = &[
        ("CR", &[""]),
        ("DR", &[""]),
        ("XCR", &[""]),
        ("XMM", &[""]),
        ("YMM", &[""]),
        ("ZMM", &[""]),
        ("TMM", &[""]),
        ("MM", &[""]),
        ("ST", &[""]),
        ("K", &[""]),
        ("BND", &[""]),
        ("R", &["", "D", "W", "B", "L"]),
    ];
    let upper = entity.to_ascii_uppercase();
    if NAMED.contains(&upper.as_str()) {
        return true;
    }
    let upper = upper.replace(['(', ')'], "");
    NUMBERED.iter().any(|(prefix, suffixes)| {
        upper.strip_prefix(prefix).is_some_and(|rest| {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && suffixes.contains(&&rest[digits..])
        })
    })
}

/// Architectural exception mnemonics such as `#GP`, `#PF`, or `#GP(0)`.
fn is_exception_mnemonic(entity: &str) -> bool {
    let Some(rest) = entity.strip_prefix('#') else {
        return false;
    };
    let (name, error_code) = rest.split_once('(').unwrap_or((rest, ""));
    name.len() == 2
        && name.bytes().all(|byte| byte.is_ascii_alphabetic())
        && (error_code.is_empty() || error_code.ends_with(')'))
}

/// `REG.FIELD` or `REG[bits]` references such as `CR4.VMXE`, `IA32_EFER.LME`, or `CR3[51:12]`.
fn is_bitfield_reference(entity: &str) -> bool {
    let upper = entity.trim().to_ascii_uppercase();
    if upper.starts_with("CPUID") {
        return upper
            .split_once(':')
            .is_some_and(|(_, output)| output.contains(['.', '[']));
    }
    let split = upper.find(['.', '[']);
    let Some(index) = split else {
        return false;
    };
    let (register, field) = upper.split_at(index);
    let valid_register = register.starts_with(|character: char| character.is_ascii_alphabetic())
        && register.bytes().all(is_identifier_byte);
    let field = &field[1..];
    valid_register
        && if upper.as_bytes()[index] == b'[' {
            field.len() > 1 && field.ends_with(']')
        } else {
            field.starts_with(|character: char| character.is_ascii_alphanumeric())
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

#[cfg(test)]
mod tests {
    use crate::domain::{
        Vendor,
        block::ContentClass,
        chunk::{ChunkKind, SearchChunk},
        source::SourceSpan,
    };

    use super::{EntityKind, kind_matches};

    fn chunk(heading_path: &[&str], text: &str) -> SearchChunk {
        SearchChunk {
            chunk_id: "chk:test".into(),
            document_id: "doc:test".into(),
            section_id: "sec:test".into(),
            vendor: Vendor::Intel,
            heading_path: heading_path
                .iter()
                .map(|&heading| heading.to_owned())
                .collect(),
            source_block_ids: Vec::new(),
            kind: ChunkKind::Prose,
            content_class: ContentClass::Substantive,
            text: text.into(),
            token_count: 1,
            content_hash: String::new(),
            symbols: Vec::new(),
            span: SourceSpan {
                byte_start: 0,
                byte_end: 1,
                line_start: 1,
                line_end: 1,
                printed_page: None,
            },
        }
    }

    #[test]
    fn instruction_kind_requires_an_instruction_reference_heading() {
        let intel = chunk(
            &["MOVSX/MOVSXD—Move With Sign-Extension", "Description"],
            "MOVSXD sign-extends a doubleword.",
        );
        let amd = chunk(
            &["VMRUN Run Virtual Machine"],
            "VMRUN starts a guest. It uses the VM_HSAVE_PA MSR.",
        );
        let msr = chunk(
            &["IA32_EFER Extended Feature Enable Register"],
            "IA32_EFER is a model-specific register.",
        );
        let chapter = chunk(
            &["15 Secure Virtual Machine", "15.5 VMRUN Instruction"],
            "VMRUN is described here.",
        );
        assert!(kind_matches(EntityKind::Instruction, "MOVSXD", &intel));
        assert!(kind_matches(EntityKind::Instruction, "vmrun", &amd));
        assert!(!kind_matches(EntityKind::Instruction, "IA32_EFER", &msr));
        assert!(!kind_matches(EntityKind::Instruction, "VMRUN", &chapter));
        assert!(!kind_matches(EntityKind::Msr, "VMRUN", &amd));
    }

    #[test]
    fn typed_kinds_accept_conventional_names_and_marker_co_occurrence() {
        let msr = chunk(
            &["Architectural MSRs"],
            "EFER (MSR C000_0080h) enables long mode. Set CR4.VMXE before VMXON. #GP(0) is raised otherwise. CPUID.01H:ECX.VMX[bit 5] reports VMX support.",
        );
        assert!(kind_matches(
            EntityKind::Msr,
            "IA32_EFER",
            &chunk(&[], "IA32_EFER")
        ));
        assert!(kind_matches(EntityKind::Msr, "C000_0080h", &msr));
        assert!(kind_matches(EntityKind::Msr, "EFER", &msr));
        assert!(kind_matches(EntityKind::Register, "EFER", &msr));
        assert!(kind_matches(EntityKind::Register, "CR4", &msr));
        assert!(!kind_matches(EntityKind::Instruction, "CR4", &msr));
        assert!(kind_matches(EntityKind::Bitfield, "CR4.VMXE", &msr));
        assert!(!kind_matches(EntityKind::Bitfield, "CR4", &msr));
        assert!(kind_matches(EntityKind::Exception, "#GP(0)", &msr));
        assert!(!kind_matches(EntityKind::Exception, "CR4.VMXE", &msr));
        assert!(kind_matches(
            EntityKind::Cpuid,
            "CPUID.01H:ECX.VMX[bit 5]",
            &msr
        ));
        assert!(kind_matches(
            EntityKind::Bitfield,
            "CPUID.01H:ECX.VMX[bit 5]",
            &msr
        ));
        assert!(!kind_matches(
            EntityKind::Cpuid,
            "C000_0080h",
            &chunk(&[], "C000_0080h")
        ));
        assert!(!kind_matches(EntityKind::Msr, "IA32_EFER", &msr));
    }
}
