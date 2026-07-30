use std::{
    collections::{HashMap, HashSet},
    path::Path,
    str::FromStr,
    sync::Arc,
};

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, types::Type};

use crate::{
    domain::{
        Vendor,
        block::{BlockKind, ContentClass, SourceBlock},
        chunk::{ChunkKind, SearchChunk},
        document::DocumentMeta,
        reference::{ReferenceKind, ReferenceRecord},
        source::SourceSpan,
    },
    ingest::{
        DiagramEdge, DiagramNode, ExtractedDiagram, ExtractedTable, ExtractionWarning,
        IngestWarning, ParsedDocument, SectionNode,
    },
};

use super::{
    CatalogCounts, CatalogDocument, CatalogError, OutlineNode, ReferenceDirection,
    ResolvedReference, SectionView, TablePage, VectorChunkMetadata, VectorReuseRecord,
    schema::CATALOG_SCHEMA_VERSION,
};

#[derive(Clone)]
pub struct CatalogReader {
    connection: Arc<Mutex<Connection>>,
}

impl CatalogReader {
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA query_only=ON;")?;
        let schema: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| CatalogError::Integrity("missing catalog schema version".into()))?;
        let found = schema.parse::<u32>().map_err(|_| {
            CatalogError::Integrity(format!("invalid catalog schema version {schema:?}"))
        })?;
        if found != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchema {
                found,
                supported: CATALOG_SCHEMA_VERSION,
            });
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn document(&self, id: &str) -> Result<Option<CatalogDocument>, CatalogError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT id, vendor, archive_id, archive_sha256, entry_path, content_hash, raw_len, title, revision
                 FROM documents WHERE id=?1",
                [id],
                document_from_row,
            )
            .optional()?)
    }

    pub fn documents(&self) -> Result<Vec<CatalogDocument>, CatalogError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, vendor, archive_id, archive_sha256, entry_path, content_hash,
                    raw_len, title, revision
             FROM documents ORDER BY vendor, id",
        )?;
        Ok(statement
            .query_map([], document_from_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn outline(
        &self,
        document_id: &str,
        root: Option<&str>,
        depth: u8,
    ) -> Result<Vec<OutlineNode>, CatalogError> {
        if depth > 8 {
            return Err(CatalogError::InvalidRequest(
                "outline depth cannot exceed 8".into(),
            ));
        }
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, document_id, parent_id, heading, heading_path_json, level, ordinal, printed_page,
                    byte_start, byte_end, line_start, line_end
             FROM sections WHERE document_id=?1 ORDER BY byte_start, level, ordinal",
        )?;
        let sections = statement
            .query_map([document_id], section_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let parents = sections
            .iter()
            .map(|section| {
                (
                    section.section_id.as_str(),
                    section.parent_section_id.as_deref(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut output = Vec::new();
        for section in &sections {
            let relative_depth = relative_depth(&section.section_id, root, &parents);
            if relative_depth.is_some_and(|relative| relative <= depth) {
                output.push(OutlineNode {
                    section: section.clone(),
                    relative_depth: relative_depth.unwrap(),
                });
            }
        }
        Ok(output)
    }

    pub fn section(&self, id: &str) -> Result<Option<SectionView>, CatalogError> {
        let connection = self.connection.lock();
        let section = connection
            .query_row(
                "SELECT id, document_id, parent_id, heading, heading_path_json, level, ordinal, printed_page,
                        byte_start, byte_end, line_start, line_end
                 FROM sections WHERE id=?1",
                [id],
                section_from_row,
            )
            .optional()?;
        let Some(section) = section else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT id, document_id, section_id, kind, content_class, heading_path_json,
                    raw_text, normalized_text, printed_page, byte_start, byte_end, line_start, line_end
             FROM blocks WHERE section_id=?1 ORDER BY ordinal",
        )?;
        let blocks = statement
            .query_map([id], block_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(SectionView { section, blocks }))
    }

    pub fn document_id_for_section(&self, id: &str) -> Result<Option<String>, CatalogError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT document_id FROM sections WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn block(&self, id: &str) -> Result<Option<SourceBlock>, CatalogError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT id, document_id, section_id, kind, content_class, heading_path_json,
                        raw_text, normalized_text, printed_page, byte_start, byte_end,
                        line_start, line_end
                 FROM blocks WHERE id=?1",
                [id],
                block_from_row,
            )
            .optional()?)
    }

    pub fn table(
        &self,
        id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Option<TablePage>, CatalogError> {
        enforce_limit(limit, 200, "table row")?;
        let connection = self.connection.lock();
        let metadata = connection
            .query_row(
                "SELECT t.id, t.block_id, t.caption, t.headers_json, t.row_count, b.raw_text
                 FROM tables t JOIN blocks b ON b.id=t.block_id
                 WHERE t.block_id=?1 OR t.id=?1 LIMIT 1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        json_cell::<Vec<String>>(row, 3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((table_id, block_id, caption, headers, total_rows, raw_source)) = metadata else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT cells_json FROM table_rows WHERE block_id=?1 AND row_index>=?2
             ORDER BY row_index LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![block_id, i64::from(offset), i64::from(limit)],
                |row| json_cell::<Vec<String>>(row, 0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let total_rows = u32_from_i64(total_rows, 4, "table row count")?;
        Ok(Some(TablePage {
            table_id,
            block_id,
            caption,
            headers,
            has_more: offset.saturating_add(rows.len() as u32) < total_rows,
            rows,
            total_rows,
            offset,
            limit,
            raw_source,
        }))
    }

    pub fn diagram(&self, id: &str) -> Result<Option<ExtractedDiagram>, CatalogError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT id, block_id, caption, direction, mermaid, nodes_json, edges_json,
                        subgraphs_json, search_labels_json, warnings_json
                 FROM diagrams WHERE block_id=?1 OR id=?1 LIMIT 1",
                [id],
                |row| {
                    Ok(ExtractedDiagram {
                        diagram_id: row.get(0)?,
                        source_block_id: row.get(1)?,
                        caption: row.get(2)?,
                        direction: row.get(3)?,
                        raw_source: row.get(4)?,
                        nodes: json_cell::<Vec<DiagramNode>>(row, 5)?,
                        edges: json_cell::<Vec<DiagramEdge>>(row, 6)?,
                        subgraphs: json_cell::<Vec<String>>(row, 7)?,
                        search_labels: json_cell::<Vec<String>>(row, 8)?,
                        warnings: json_cell::<Vec<ExtractionWarning>>(row, 9)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn references(
        &self,
        id: &str,
        direction: ReferenceDirection,
        limit: u32,
    ) -> Result<Vec<ResolvedReference>, CatalogError> {
        enforce_limit(limit, 201, "reference")?;
        let sql = match direction {
            ReferenceDirection::Outgoing => {
                "SELECT r.id, r.source_block_id, r.kind, r.label, r.normalized_key,
                        r.target_document_id, r.target_id, r.candidates_json, r.resolved,
                        b.document_id, b.section_id
                 FROM refs r JOIN blocks b ON b.id=r.source_block_id
                 WHERE r.source_block_id=?1 OR b.section_id=?1 OR b.document_id=?1
                 ORDER BY r.id LIMIT ?2"
            }
            ReferenceDirection::Incoming => {
                "SELECT r.id, r.source_block_id, r.kind, r.label, r.normalized_key,
                        r.target_document_id, r.target_id, r.candidates_json, r.resolved,
                        b.document_id, b.section_id
                 FROM refs r JOIN blocks b ON b.id=r.source_block_id
                 WHERE r.target_id=?1 OR r.target_document_id=?1
                 ORDER BY r.id LIMIT ?2"
            }
        };
        let connection = self.connection.lock();
        let mut statement = connection.prepare(sql)?;
        Ok(statement
            .query_map(params![id, i64::from(limit)], resolved_reference_from_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn chunk(&self, id: &str) -> Result<Option<SearchChunk>, CatalogError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT id, document_id, section_id, vendor, heading_path_json, block_ids_json,
                        kind, content_class, text, token_count, symbols_json, content_hash,
                        printed_page, byte_start, byte_end, line_start, line_end
                 FROM chunks WHERE id=?1",
                [id],
                chunk_from_row,
            )
            .optional()?)
    }

    pub fn chunks_by_ids(&self, ids: &[String]) -> Result<Vec<SearchChunk>, CatalogError> {
        if ids.len() > 256 {
            return Err(CatalogError::InvalidRequest(
                "at most 256 chunk IDs can be loaded at once".into(),
            ));
        }
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, document_id, section_id, vendor, heading_path_json, block_ids_json,
                    kind, content_class, text, token_count, symbols_json, content_hash,
                    printed_page, byte_start, byte_end, line_start, line_end
             FROM chunks WHERE id=?1",
        )?;
        let mut chunks = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(chunk) = statement.query_row([id], chunk_from_row).optional()? {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    pub fn chunks_with_vector_rows(&self) -> Result<Vec<(u64, SearchChunk)>, CatalogError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT vector_row, id, document_id, section_id, vendor, heading_path_json,
                    block_ids_json, kind, content_class, text, token_count, symbols_json,
                    content_hash, printed_page, byte_start, byte_end, line_start, line_end
             FROM chunks ORDER BY vector_row",
        )?;
        Ok(statement
            .query_map([], |row| {
                let vector_row = u64_from_i64(row.get(0)?, 0, "vector row")?;
                Ok((vector_row, chunk_from_row_offset(row, 1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn vector_metadata(&self) -> Result<Vec<VectorChunkMetadata>, CatalogError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT vector_row, id, document_id, vendor, kind
             FROM chunks ORDER BY vector_row",
        )?;
        Ok(statement
            .query_map([], |row| {
                let vendor_text: String = row.get(3)?;
                let vendor = Vendor::from_str(&vendor_text)
                    .map_err(|error| conversion_error(3, error.to_owned()))?;
                Ok(VectorChunkMetadata {
                    vector_row: u64_from_i64(row.get(0)?, 0, "vector row")?,
                    chunk_id: row.get(1)?,
                    document_id: row.get(2)?,
                    vendor,
                    kind: parse_chunk_kind(&row.get::<_, String>(4)?, 4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn vector_row(&self, chunk_id: &str) -> Result<Option<u64>, CatalogError> {
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT vector_row FROM chunks WHERE id=?1",
                [chunk_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        row.map(|value| u64_from_i64(value, 0, "vector row"))
            .transpose()
            .map_err(CatalogError::from)
    }

    pub fn parsed_document(
        &self,
        document_id: &str,
    ) -> Result<Option<ParsedDocument>, CatalogError> {
        let connection = self.connection.lock();
        let exists = connection
            .query_row("SELECT 1 FROM documents WHERE id=?1", [document_id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Ok(None);
        }
        Ok(Some(parsed_document_from_connection(
            &connection,
            document_id,
        )?))
    }

    pub fn vector_reuse_records(&self) -> Result<Vec<VectorReuseRecord>, CatalogError> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare("SELECT content_hash, vector_row FROM chunks ORDER BY vector_row")?;
        Ok(statement
            .query_map([], |row| {
                Ok(VectorReuseRecord {
                    content_hash: row.get(0)?,
                    vector_row: u64_from_i64(row.get(1)?, 1, "vector row")?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn counts(&self) -> Result<CatalogCounts, CatalogError> {
        Ok(CatalogCounts {
            archives: self.count("archives")?,
            documents: self.count("documents")?,
            sections: self.count("sections")?,
            blocks: self.count("blocks")?,
            chunks: self.count("chunks")?,
            tables: self.count("tables")?,
            diagrams: self.count("diagrams")?,
            references: self.count("refs")?,
            warnings: self.count("warnings")?,
            vectors: self.count("vectors")?,
        })
    }

    pub fn integrity_check(&self) -> Result<(), CatalogError> {
        let connection = self.connection.lock();
        let result: String =
            connection.query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))?;
        if result != "ok" {
            return Err(CatalogError::Integrity(format!(
                "SQLite integrity check failed: {result}"
            )));
        }
        let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
        if foreign_keys.query([])?.next()?.is_some() {
            return Err(CatalogError::Integrity(
                "SQLite foreign-key check failed".into(),
            ));
        }
        let (count, distinct, minimum, maximum): (i64, i64, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT vector_row), MIN(vector_row), MAX(vector_row)
                 FROM chunks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        if count != distinct || (count > 0 && (minimum != Some(0) || maximum != Some(count - 1))) {
            return Err(CatalogError::Integrity(
                "vector rows are not contiguous and unique".into(),
            ));
        }
        Ok(())
    }

    pub fn warning_count(&self) -> Result<u64, CatalogError> {
        self.count("warnings")
    }

    pub fn document_count(&self) -> Result<u64, CatalogError> {
        self.count("documents")
    }

    pub fn chunk_count(&self) -> Result<u64, CatalogError> {
        self.count("chunks")
    }

    fn count(&self, table: &'static str) -> Result<u64, CatalogError> {
        let sql = match table {
            "archives" => "SELECT COUNT(DISTINCT archive_id) FROM documents",
            "documents" => "SELECT COUNT(*) FROM documents",
            "sections" => "SELECT COUNT(*) FROM sections",
            "blocks" => "SELECT COUNT(*) FROM blocks",
            "chunks" | "vectors" => "SELECT COUNT(*) FROM chunks",
            "tables" => "SELECT COUNT(*) FROM tables",
            "diagrams" => "SELECT COUNT(*) FROM diagrams",
            "refs" => "SELECT COUNT(*) FROM refs",
            "warnings" => "SELECT COUNT(*) FROM warnings",
            _ => return Err(CatalogError::Integrity("unknown count table".into())),
        };
        let connection = self.connection.lock();
        let count = connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
        Ok(u64_from_i64(count, 0, "row count")?)
    }
}

fn parsed_document_from_connection(
    connection: &Connection,
    document_id: &str,
) -> Result<ParsedDocument, CatalogError> {
    let sections = {
        let mut statement = connection.prepare(
            "SELECT id, document_id, parent_id, heading, heading_path_json, level, ordinal,
                    printed_page, byte_start, byte_end, line_start, line_end
             FROM sections WHERE document_id=?1 AND level > 0
             ORDER BY byte_start, level, ordinal",
        )?;
        statement
            .query_map([document_id], section_from_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let blocks = {
        let mut statement = connection.prepare(
            "SELECT id, document_id, section_id, kind, content_class, heading_path_json,
                    raw_text, normalized_text, printed_page, byte_start, byte_end,
                    line_start, line_end
             FROM blocks WHERE document_id=?1 ORDER BY ordinal",
        )?;
        statement
            .query_map([document_id], block_from_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let table_metadata = {
        let mut statement = connection.prepare(
            "SELECT t.id, t.block_id, t.caption, t.headers_json, t.row_count, b.raw_text
             FROM tables t JOIN blocks b ON b.id=t.block_id
             WHERE b.document_id=?1 ORDER BY b.ordinal",
        )?;
        statement
            .query_map([document_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    json_cell::<Vec<String>>(row, 3)?,
                    u64_from_i64(row.get(4)?, 4, "table row count")?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut tables = Vec::with_capacity(table_metadata.len());
    for (table_id, block_id, caption, headers, expected_rows, raw_source) in table_metadata {
        let rows = {
            let mut statement = connection.prepare(
                "SELECT cells_json FROM table_rows WHERE block_id=?1 ORDER BY row_index",
            )?;
            statement
                .query_map([&block_id], |row| json_cell::<Vec<String>>(row, 0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if rows.len() as u64 != expected_rows {
            return Err(CatalogError::Integrity(format!(
                "table {table_id} row count mismatch"
            )));
        }
        let warnings = {
            let mut statement = connection
                .prepare("SELECT code, message FROM warnings WHERE block_id=?1 ORDER BY id")?;
            statement
                .query_map([&block_id], |row| {
                    Ok(ExtractionWarning {
                        code: row.get(0)?,
                        message: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        tables.push(ExtractedTable {
            table_id,
            source_block_id: block_id,
            caption,
            headers,
            rows,
            raw_source,
            warnings,
        });
    }
    let diagrams = {
        let mut statement = connection.prepare(
            "SELECT d.id, d.block_id, d.direction, d.caption, d.nodes_json, d.edges_json,
                    d.subgraphs_json, d.search_labels_json, d.mermaid, d.warnings_json
             FROM diagrams d JOIN blocks b ON b.id=d.block_id
             WHERE b.document_id=?1 ORDER BY b.ordinal",
        )?;
        statement
            .query_map([document_id], |row| {
                Ok(ExtractedDiagram {
                    diagram_id: row.get(0)?,
                    source_block_id: row.get(1)?,
                    direction: row.get(2)?,
                    caption: row.get(3)?,
                    nodes: json_cell::<Vec<DiagramNode>>(row, 4)?,
                    edges: json_cell::<Vec<DiagramEdge>>(row, 5)?,
                    subgraphs: json_cell::<Vec<String>>(row, 6)?,
                    search_labels: json_cell::<Vec<String>>(row, 7)?,
                    raw_source: row.get(8)?,
                    warnings: json_cell::<Vec<ExtractionWarning>>(row, 9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let references = {
        let mut statement = connection.prepare(
            "SELECT r.id, r.source_block_id, r.kind, r.label, r.normalized_key,
                    r.target_document_id, r.target_id, r.candidates_json, r.resolved,
                    b.document_id, b.section_id
             FROM refs r JOIN blocks b ON b.id=r.source_block_id
             WHERE b.document_id=?1
             ORDER BY b.ordinal, instr(b.raw_text, r.label), r.id",
        )?;
        statement
            .query_map([document_id], resolved_reference_from_row)?
            .map(|result| result.map(|resolved| resolved.record))
            .collect::<Result<Vec<_>, _>>()?
    };
    let warnings = {
        let mut statement = connection.prepare(
            "SELECT code, message, byte_start, byte_end, line_start, line_end
             FROM warnings
             WHERE document_id=?1 AND block_id IS NULL ORDER BY id",
        )?;
        statement
            .query_map([document_id], |row| {
                Ok(IngestWarning {
                    code: row.get(0)?,
                    message: row.get(1)?,
                    span: SourceSpan {
                        byte_start: u64_from_i64(row.get(2)?, 2, "warning byte_start")?,
                        byte_end: u64_from_i64(row.get(3)?, 3, "warning byte_end")?,
                        line_start: u32_from_i64(row.get(4)?, 4, "warning line_start")?,
                        line_end: u32_from_i64(row.get(5)?, 5, "warning line_end")?,
                        printed_page: None,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(ParsedDocument {
        document_id: document_id.to_owned(),
        sections,
        blocks,
        tables,
        diagrams,
        references,
        warnings,
    })
}

fn document_from_row(row: &Row<'_>) -> rusqlite::Result<CatalogDocument> {
    let vendor_text: String = row.get(1)?;
    let vendor =
        Vendor::from_str(&vendor_text).map_err(|error| conversion_error(1, error.to_owned()))?;
    let raw_len = u64_from_i64(row.get(6)?, 6, "document raw length")?;
    Ok(CatalogDocument {
        meta: DocumentMeta {
            document_id: row.get(0)?,
            vendor,
            archive_id: row.get(2)?,
            archive_sha256: row.get(3)?,
            entry_path: row.get(4)?,
            content_sha256: row.get(5)?,
            byte_len: raw_len,
        },
        title: row.get(7)?,
        revision: row.get(8)?,
    })
}

fn section_from_row(row: &Row<'_>) -> rusqlite::Result<SectionNode> {
    Ok(SectionNode {
        section_id: row.get(0)?,
        parent_section_id: row.get(2)?,
        heading: row.get(3)?,
        heading_path: json_cell(row, 4)?,
        level: u8_from_i64(row.get(5)?, 5, "section level")?,
        ordinal: u32_from_i64(row.get(6)?, 6, "section ordinal")?,
        printed_page: row.get(7)?,
        span: SourceSpan {
            byte_start: u64_from_i64(row.get(8)?, 8, "section byte_start")?,
            byte_end: u64_from_i64(row.get(9)?, 9, "section byte_end")?,
            line_start: u32_from_i64(row.get(10)?, 10, "section line_start")?,
            line_end: u32_from_i64(row.get(11)?, 11, "section line_end")?,
            printed_page: row.get(7)?,
        },
    })
}

fn block_from_row(row: &Row<'_>) -> rusqlite::Result<SourceBlock> {
    let printed_page = row.get(8)?;
    Ok(SourceBlock {
        block_id: row.get(0)?,
        document_id: row.get(1)?,
        section_id: row.get(2)?,
        kind: parse_block_kind(&row.get::<_, String>(3)?, 3)?,
        content_class: parse_content_class(&row.get::<_, String>(4)?, 4)?,
        heading_path: json_cell(row, 5)?,
        raw_source: row.get(6)?,
        normalized_text: row.get(7)?,
        span: SourceSpan {
            printed_page,
            byte_start: u64_from_i64(row.get(9)?, 9, "block byte_start")?,
            byte_end: u64_from_i64(row.get(10)?, 10, "block byte_end")?,
            line_start: u32_from_i64(row.get(11)?, 11, "block line_start")?,
            line_end: u32_from_i64(row.get(12)?, 12, "block line_end")?,
        },
    })
}

fn chunk_from_row(row: &Row<'_>) -> rusqlite::Result<SearchChunk> {
    chunk_from_row_offset(row, 0)
}

fn chunk_from_row_offset(row: &Row<'_>, offset: usize) -> rusqlite::Result<SearchChunk> {
    let vendor_text: String = row.get(offset + 3)?;
    let vendor = Vendor::from_str(&vendor_text)
        .map_err(|error| conversion_error(offset + 3, error.to_owned()))?;
    let printed_page = row.get(offset + 12)?;
    Ok(SearchChunk {
        chunk_id: row.get(offset)?,
        document_id: row.get(offset + 1)?,
        section_id: row.get(offset + 2)?,
        vendor,
        heading_path: json_cell(row, offset + 4)?,
        source_block_ids: json_cell(row, offset + 5)?,
        kind: parse_chunk_kind(&row.get::<_, String>(offset + 6)?, offset + 6)?,
        content_class: parse_content_class(&row.get::<_, String>(offset + 7)?, offset + 7)?,
        text: row.get(offset + 8)?,
        token_count: u32_from_i64(row.get(offset + 9)?, offset + 9, "chunk token count")?,
        symbols: json_cell(row, offset + 10)?,
        content_hash: row.get(offset + 11)?,
        span: SourceSpan {
            printed_page,
            byte_start: u64_from_i64(row.get(offset + 13)?, offset + 13, "chunk byte_start")?,
            byte_end: u64_from_i64(row.get(offset + 14)?, offset + 14, "chunk byte_end")?,
            line_start: u32_from_i64(row.get(offset + 15)?, offset + 15, "chunk line_start")?,
            line_end: u32_from_i64(row.get(offset + 16)?, offset + 16, "chunk line_end")?,
        },
    })
}

fn resolved_reference_from_row(row: &Row<'_>) -> rusqlite::Result<ResolvedReference> {
    Ok(ResolvedReference {
        record: ReferenceRecord {
            reference_id: row.get(0)?,
            source_block_id: row.get(1)?,
            kind: parse_reference_kind(&row.get::<_, String>(2)?, 2)?,
            label: row.get(3)?,
            normalized_key: row.get(4)?,
            target_document_id: row.get(5)?,
            target_id: row.get(6)?,
            candidates: json_cell(row, 7)?,
            resolved: row.get::<_, i64>(8)? != 0,
        },
        source_document_id: row.get(9)?,
        source_section_id: row.get(10)?,
    })
}

fn relative_depth<'a>(
    section_id: &'a str,
    root: Option<&str>,
    parents: &HashMap<&'a str, Option<&'a str>>,
) -> Option<u8> {
    let mut current = section_id;
    let mut distance = 0_u8;
    let mut seen = HashSet::new();
    loop {
        if root.is_some_and(|root| current == root) {
            return Some(distance);
        }
        if !seen.insert(current) {
            return None;
        }
        let parent = parents.get(current).copied().flatten();
        match (root, parent) {
            (None, None) => return Some(distance),
            (Some(_), None) => return None,
            (_, Some(parent)) => {
                current = parent;
                distance = distance.checked_add(1)?;
            }
        }
    }
}

fn enforce_limit(limit: u32, maximum: u32, noun: &str) -> Result<(), CatalogError> {
    if limit == 0 || limit > maximum {
        return Err(CatalogError::InvalidRequest(format!(
            "{noun} limit must be between 1 and {maximum}"
        )));
    }
    Ok(())
}

fn json_cell<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

fn parse_block_kind(value: &str, index: usize) -> rusqlite::Result<BlockKind> {
    match value {
        "prose" => Ok(BlockKind::Prose),
        "list" => Ok(BlockKind::List),
        "code" => Ok(BlockKind::Code),
        "table" => Ok(BlockKind::Table),
        "diagram" => Ok(BlockKind::Diagram),
        "quote" => Ok(BlockKind::Quote),
        "caption" => Ok(BlockKind::Caption),
        _ => Err(conversion_error(
            index,
            format!("unknown block kind {value}"),
        )),
    }
}

fn parse_chunk_kind(value: &str, index: usize) -> rusqlite::Result<ChunkKind> {
    match value {
        "prose" => Ok(ChunkKind::Prose),
        "list" => Ok(ChunkKind::List),
        "code" => Ok(ChunkKind::Code),
        "table" => Ok(ChunkKind::Table),
        "diagram" => Ok(ChunkKind::Diagram),
        _ => Err(conversion_error(
            index,
            format!("unknown chunk kind {value}"),
        )),
    }
}

fn parse_content_class(value: &str, index: usize) -> rusqlite::Result<ContentClass> {
    match value {
        "substantive" => Ok(ContentClass::Substantive),
        "front_matter" => Ok(ContentClass::FrontMatter),
        "contents" => Ok(ContentClass::Contents),
        "legal" => Ok(ContentClass::Legal),
        "revision_history" => Ok(ContentClass::RevisionHistory),
        "page_furniture" => Ok(ContentClass::PageFurniture),
        _ => Err(conversion_error(
            index,
            format!("unknown content class {value}"),
        )),
    }
}

fn parse_reference_kind(value: &str, index: usize) -> rusqlite::Result<ReferenceKind> {
    match value {
        "section" => Ok(ReferenceKind::Section),
        "table" => Ok(ReferenceKind::Table),
        "figure" => Ok(ReferenceKind::Figure),
        "document" => Ok(ReferenceKind::Document),
        _ => Err(conversion_error(
            index,
            format!("unknown reference kind {value}"),
        )),
    }
}

fn u64_from_i64(value: i64, index: usize, field: &str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| conversion_error(index, format!("negative {field}")))
}

fn u32_from_i64(value: i64, index: usize, field: &str) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| conversion_error(index, format!("invalid {field}")))
}

fn u8_from_i64(value: i64, index: usize, field: &str) -> rusqlite::Result<u8> {
    u8::try_from(value).map_err(|_| conversion_error(index, format!("invalid {field}")))
}

fn conversion_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}
