use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use rusqlite::{Connection, OpenFlags, Transaction, params};

use crate::{
    domain::{
        block::{BlockKind, ContentClass},
        chunk::{ChunkKind, SearchChunk},
        document::ArchiveDocument,
        reference::ReferenceKind,
        source::SourceSpan,
    },
    ingest::{ParsedDocument, SectionNode},
};

use super::{
    CatalogError,
    schema::{CATALOG_SCHEMA_VERSION, SCHEMA_SQL},
};

pub struct CatalogWriter {
    connection: Connection,
}

impl CatalogWriter {
    pub fn create(path: &Path) -> Result<Self, CatalogError> {
        if path.exists() {
            return Err(CatalogError::AlreadyExists(path.to_path_buf()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CatalogError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF;
             PRAGMA temp_store=MEMORY;",
        )?;
        connection.execute_batch(SCHEMA_SQL)?;
        connection.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
            [CATALOG_SCHEMA_VERSION.to_string()],
        )?;
        Ok(Self { connection })
    }

    pub fn write_document(
        &mut self,
        document: &ArchiveDocument,
        parsed: &ParsedDocument,
        chunks: &[SearchChunk],
        first_vector_row: u64,
    ) -> Result<u64, CatalogError> {
        if parsed.document_id != document.meta.document_id {
            return Err(CatalogError::Integrity(format!(
                "parsed document {} does not match source {}",
                parsed.document_id, document.meta.document_id
            )));
        }
        let transaction = self.connection.transaction()?;
        insert_document(&transaction, document, parsed)?;
        insert_sections(&transaction, document, parsed)?;
        insert_blocks(&transaction, parsed)?;
        insert_chunks(&transaction, chunks, first_vector_row)?;
        insert_tables(&transaction, parsed)?;
        insert_diagrams(&transaction, parsed)?;
        insert_references(&transaction, parsed)?;
        insert_warnings(&transaction, document, parsed)?;
        ensure_foreign_keys(&transaction)?;
        transaction.commit()?;
        first_vector_row
            .checked_add(chunks.len() as u64)
            .ok_or_else(|| CatalogError::Integrity("vector row overflow".into()))
    }

    pub fn finish(self) -> Result<(), CatalogError> {
        ensure_foreign_keys(&self.connection)?;
        self.connection.execute_batch("PRAGMA optimize;")?;
        Ok(())
    }
}

fn insert_document(
    transaction: &Transaction<'_>,
    document: &ArchiveDocument,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    let title = parsed
        .sections
        .first()
        .map(|section| section.heading.clone())
        .unwrap_or_else(|| document.meta.entry_path.clone());
    let revision = revision_from_path(&document.meta.entry_path);
    transaction.execute(
        "INSERT INTO documents(id, vendor, archive_id, archive_sha256, entry_path, title, revision, content_hash, raw_len)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            document.meta.document_id,
            document.meta.vendor.to_string(),
            document.meta.archive_id,
            document.meta.archive_sha256,
            document.meta.entry_path,
            title,
            revision,
            document.meta.content_sha256,
            to_i64(document.meta.byte_len, "document raw length")?,
        ],
    )?;
    Ok(())
}

fn insert_sections(
    transaction: &Transaction<'_>,
    document: &ArchiveDocument,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    let mut inserted = HashSet::with_capacity(parsed.sections.len() + 1);
    for section in &parsed.sections {
        insert_section(transaction, &document.meta.document_id, section)?;
        inserted.insert(section.section_id.as_str());
    }
    let synthetic = parsed
        .blocks
        .iter()
        .map(|block| block.section_id.as_str())
        .filter(|section_id| !inserted.contains(section_id))
        .collect::<HashSet<_>>();
    for section_id in synthetic {
        transaction.execute(
            "INSERT INTO sections(id, document_id, parent_id, heading, heading_path_json, level, ordinal, printed_page, byte_start, byte_end, line_start, line_end)
             VALUES (?1, ?2, NULL, 'Front matter', '[]', 0, 0, NULL, 0, ?3, 1, ?4)",
            params![
                section_id,
                document.meta.document_id,
                to_i64(document.meta.byte_len, "front-matter byte length")?,
                document.source.lines().count().max(1) as i64,
            ],
        )?;
    }
    Ok(())
}

fn insert_section(
    transaction: &Transaction<'_>,
    document_id: &str,
    section: &SectionNode,
) -> Result<(), CatalogError> {
    transaction.execute(
        "INSERT INTO sections(id, document_id, parent_id, heading, heading_path_json, level, ordinal, printed_page, byte_start, byte_end, line_start, line_end)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            section.section_id,
            document_id,
            section.parent_section_id,
            section.heading,
            json(&section.heading_path)?,
            i64::from(section.level),
            i64::from(section.ordinal),
            section.printed_page,
            to_i64(section.span.byte_start, "section byte_start")?,
            to_i64(section.span.byte_end, "section byte_end")?,
            i64::from(section.span.line_start),
            i64::from(section.span.line_end),
        ],
    )?;
    Ok(())
}

fn insert_blocks(
    transaction: &Transaction<'_>,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    let captions = parsed
        .tables
        .iter()
        .filter_map(|table| {
            table
                .caption
                .as_deref()
                .map(|caption| (table.source_block_id.as_str(), caption))
        })
        .chain(parsed.diagrams.iter().filter_map(|diagram| {
            diagram
                .caption
                .as_deref()
                .map(|caption| (diagram.source_block_id.as_str(), caption))
        }))
        .collect::<HashMap<_, _>>();
    for (ordinal, block) in parsed.blocks.iter().enumerate() {
        transaction.execute(
            "INSERT INTO blocks(id, document_id, section_id, kind, content_class, heading_path_json, ordinal, caption, raw_text, normalized_text, printed_page, content_hash, byte_start, byte_end, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                block.block_id,
                block.document_id,
                block.section_id,
                block_kind(block.kind),
                content_class(block.content_class),
                json(&block.heading_path)?,
                ordinal as i64,
                captions.get(block.block_id.as_str()).copied(),
                block.raw_source,
                block.normalized_text,
                block.span.printed_page,
                blake3::hash(block.raw_source.as_bytes()).to_hex().to_string(),
                to_i64(block.span.byte_start, "block byte_start")?,
                to_i64(block.span.byte_end, "block byte_end")?,
                i64::from(block.span.line_start),
                i64::from(block.span.line_end),
            ],
        )?;
    }
    Ok(())
}

fn insert_chunks(
    transaction: &Transaction<'_>,
    chunks: &[SearchChunk],
    first_vector_row: u64,
) -> Result<(), CatalogError> {
    for (offset, chunk) in chunks.iter().enumerate() {
        let vector_row = first_vector_row
            .checked_add(offset as u64)
            .ok_or_else(|| CatalogError::Integrity("vector row overflow".into()))?;
        transaction.execute(
            "INSERT INTO chunks(id, document_id, section_id, vendor, heading_path_json, block_ids_json, kind, content_class, text, token_count, symbols_json, content_hash, vector_row, printed_page, byte_start, byte_end, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                chunk.chunk_id,
                chunk.document_id,
                chunk.section_id,
                chunk.vendor.to_string(),
                json(&chunk.heading_path)?,
                json(&chunk.source_block_ids)?,
                chunk_kind(chunk.kind),
                content_class(chunk.content_class),
                chunk.text,
                i64::from(chunk.token_count),
                json(&chunk.symbols)?,
                chunk.content_hash,
                to_i64(vector_row, "vector row")?,
                chunk.span.printed_page,
                to_i64(chunk.span.byte_start, "chunk byte_start")?,
                to_i64(chunk.span.byte_end, "chunk byte_end")?,
                i64::from(chunk.span.line_start),
                i64::from(chunk.span.line_end),
            ],
        )?;
    }
    Ok(())
}

fn insert_tables(
    transaction: &Transaction<'_>,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    for table in &parsed.tables {
        transaction.execute(
            "INSERT INTO tables(block_id, id, caption, headers_json, row_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                table.source_block_id,
                table.table_id,
                table.caption,
                json(&table.headers)?,
                table.rows.len() as i64,
            ],
        )?;
        for (row_index, row) in table.rows.iter().enumerate() {
            transaction.execute(
                "INSERT INTO table_rows(block_id, row_index, cells_json) VALUES (?1, ?2, ?3)",
                params![table.source_block_id, row_index as i64, json(row)?],
            )?;
        }
    }
    Ok(())
}

fn insert_diagrams(
    transaction: &Transaction<'_>,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    for diagram in &parsed.diagrams {
        transaction.execute(
            "INSERT INTO diagrams(block_id, id, caption, direction, mermaid, nodes_json, edges_json, subgraphs_json, search_labels_json, warnings_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                diagram.source_block_id,
                diagram.diagram_id,
                diagram.caption,
                diagram.direction,
                diagram.raw_source,
                json(&diagram.nodes)?,
                json(&diagram.edges)?,
                json(&diagram.subgraphs)?,
                json(&diagram.search_labels)?,
                json(&diagram.warnings)?,
            ],
        )?;
    }
    Ok(())
}

fn insert_references(
    transaction: &Transaction<'_>,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    for reference in &parsed.references {
        transaction.execute(
            "INSERT INTO refs(id, source_block_id, kind, label, normalized_key, target_document_id, target_id, candidates_json, resolved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                reference.reference_id,
                reference.source_block_id,
                reference_kind(reference.kind),
                reference.label,
                reference.normalized_key,
                reference.target_document_id,
                reference.target_id,
                json(&reference.candidates)?,
                i64::from(reference.resolved),
            ],
        )?;
    }
    Ok(())
}

fn insert_warnings(
    transaction: &Transaction<'_>,
    document: &ArchiveDocument,
    parsed: &ParsedDocument,
) -> Result<(), CatalogError> {
    for warning in &parsed.warnings {
        insert_warning(
            transaction,
            &document.meta.document_id,
            None,
            &warning.code,
            &warning.message,
            Some(&warning.span),
        )?;
    }
    for table in &parsed.tables {
        for warning in &table.warnings {
            insert_warning(
                transaction,
                &document.meta.document_id,
                Some(&table.source_block_id),
                &warning.code,
                &warning.message,
                None,
            )?;
        }
    }
    for diagram in &parsed.diagrams {
        for warning in &diagram.warnings {
            insert_warning(
                transaction,
                &document.meta.document_id,
                Some(&diagram.source_block_id),
                &warning.code,
                &warning.message,
                None,
            )?;
        }
    }
    Ok(())
}

fn insert_warning(
    transaction: &Transaction<'_>,
    document_id: &str,
    block_id: Option<&str>,
    code: &str,
    message: &str,
    span: Option<&SourceSpan>,
) -> Result<(), CatalogError> {
    transaction.execute(
        "INSERT INTO warnings(document_id, block_id, code, message, byte_start, byte_end, line_start, line_end)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            document_id,
            block_id,
            code,
            message,
            span.map(|span| to_i64(span.byte_start, "warning byte_start")).transpose()?,
            span.map(|span| to_i64(span.byte_end, "warning byte_end")).transpose()?,
            span.map(|span| i64::from(span.line_start)),
            span.map(|span| i64::from(span.line_end)),
        ],
    )?;
    Ok(())
}

fn ensure_foreign_keys(connection: &Connection) -> Result<(), CatalogError> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let row_id: Option<i64> = row.get(1)?;
        return Err(CatalogError::Integrity(format!(
            "foreign key violation in {table} row {row_id:?}"
        )));
    }
    Ok(())
}

fn revision_from_path(path: &str) -> Option<String> {
    let start = path.find("rev-")? + 4;
    let value = &path[start..];
    Some(
        value
            .strip_suffix(".md")
            .unwrap_or(value)
            .trim_end_matches(".markdown")
            .to_owned(),
    )
}

fn json(value: &impl serde::Serialize) -> Result<String, CatalogError> {
    Ok(serde_json::to_string(value)?)
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, CatalogError> {
    i64::try_from(value)
        .map_err(|_| CatalogError::Integrity(format!("{field} exceeds SQLite INTEGER")))
}

fn block_kind(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Prose => "prose",
        BlockKind::List => "list",
        BlockKind::Code => "code",
        BlockKind::Table => "table",
        BlockKind::Diagram => "diagram",
        BlockKind::Quote => "quote",
        BlockKind::Caption => "caption",
    }
}

fn chunk_kind(kind: ChunkKind) -> &'static str {
    match kind {
        ChunkKind::Prose => "prose",
        ChunkKind::List => "list",
        ChunkKind::Code => "code",
        ChunkKind::Table => "table",
        ChunkKind::Diagram => "diagram",
    }
}

fn content_class(class: ContentClass) -> &'static str {
    match class {
        ContentClass::Substantive => "substantive",
        ContentClass::FrontMatter => "front_matter",
        ContentClass::Contents => "contents",
        ContentClass::Legal => "legal",
        ContentClass::RevisionHistory => "revision_history",
        ContentClass::PageFurniture => "page_furniture",
    }
}

fn reference_kind(kind: ReferenceKind) -> &'static str {
    match kind {
        ReferenceKind::Section => "section",
        ReferenceKind::Table => "table",
        ReferenceKind::Figure => "figure",
        ReferenceKind::Document => "document",
    }
}
