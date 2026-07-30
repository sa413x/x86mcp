pub const CATALOG_SCHEMA_VERSION: u32 = 1;

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE documents(
    id TEXT PRIMARY KEY,
    vendor TEXT NOT NULL,
    archive_id TEXT NOT NULL,
    archive_sha256 TEXT NOT NULL,
    entry_path TEXT NOT NULL,
    title TEXT NOT NULL,
    revision TEXT,
    content_hash TEXT NOT NULL,
    raw_len INTEGER NOT NULL CHECK(raw_len >= 0)
) STRICT;

CREATE TABLE sections(
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id),
    parent_id TEXT REFERENCES sections(id),
    heading TEXT NOT NULL,
    heading_path_json TEXT NOT NULL,
    level INTEGER NOT NULL CHECK(level BETWEEN 0 AND 6),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    printed_page TEXT,
    byte_start INTEGER NOT NULL CHECK(byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK(byte_end >= byte_start),
    line_start INTEGER NOT NULL CHECK(line_start >= 0),
    line_end INTEGER NOT NULL CHECK(line_end >= line_start)
) STRICT;

CREATE TABLE blocks(
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id),
    section_id TEXT NOT NULL REFERENCES sections(id),
    kind TEXT NOT NULL,
    content_class TEXT NOT NULL,
    heading_path_json TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    caption TEXT,
    raw_text TEXT NOT NULL,
    normalized_text TEXT NOT NULL,
    printed_page TEXT,
    content_hash TEXT NOT NULL,
    byte_start INTEGER NOT NULL CHECK(byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK(byte_end >= byte_start),
    line_start INTEGER NOT NULL CHECK(line_start >= 0),
    line_end INTEGER NOT NULL CHECK(line_end >= line_start)
) STRICT;

CREATE TABLE chunks(
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id),
    section_id TEXT NOT NULL REFERENCES sections(id),
    vendor TEXT NOT NULL,
    heading_path_json TEXT NOT NULL,
    block_ids_json TEXT NOT NULL,
    kind TEXT NOT NULL,
    content_class TEXT NOT NULL,
    text TEXT NOT NULL,
    token_count INTEGER NOT NULL CHECK(token_count >= 0),
    symbols_json TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    vector_row INTEGER UNIQUE CHECK(vector_row >= 0),
    printed_page TEXT,
    byte_start INTEGER NOT NULL CHECK(byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK(byte_end >= byte_start),
    line_start INTEGER NOT NULL CHECK(line_start >= 0),
    line_end INTEGER NOT NULL CHECK(line_end >= line_start)
) STRICT;

CREATE TABLE tables(
    block_id TEXT PRIMARY KEY REFERENCES blocks(id),
    id TEXT NOT NULL UNIQUE,
    caption TEXT,
    headers_json TEXT NOT NULL,
    row_count INTEGER NOT NULL CHECK(row_count >= 0)
) STRICT;

CREATE TABLE table_rows(
    block_id TEXT NOT NULL REFERENCES tables(block_id),
    row_index INTEGER NOT NULL CHECK(row_index >= 0),
    cells_json TEXT NOT NULL,
    PRIMARY KEY(block_id, row_index)
) STRICT;

CREATE TABLE diagrams(
    block_id TEXT PRIMARY KEY REFERENCES blocks(id),
    id TEXT NOT NULL UNIQUE,
    caption TEXT,
    direction TEXT,
    mermaid TEXT NOT NULL,
    nodes_json TEXT NOT NULL,
    edges_json TEXT NOT NULL,
    subgraphs_json TEXT NOT NULL,
    search_labels_json TEXT NOT NULL,
    warnings_json TEXT NOT NULL
) STRICT;

CREATE TABLE refs(
    id TEXT PRIMARY KEY,
    source_block_id TEXT NOT NULL REFERENCES blocks(id),
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    normalized_key TEXT NOT NULL,
    target_document_id TEXT,
    target_id TEXT,
    candidates_json TEXT NOT NULL,
    resolved INTEGER NOT NULL CHECK(resolved IN (0, 1))
) STRICT;

CREATE TABLE warnings(
    id INTEGER PRIMARY KEY,
    document_id TEXT REFERENCES documents(id),
    block_id TEXT REFERENCES blocks(id),
    code TEXT NOT NULL,
    message TEXT NOT NULL,
    byte_start INTEGER,
    byte_end INTEGER,
    line_start INTEGER,
    line_end INTEGER
) STRICT;

CREATE INDEX sections_document ON sections(document_id);
CREATE INDEX sections_parent ON sections(parent_id);
CREATE INDEX blocks_document ON blocks(document_id);
CREATE INDEX blocks_section ON blocks(section_id);
CREATE INDEX chunks_document ON chunks(document_id);
CREATE INDEX chunks_section ON chunks(section_id);
CREATE INDEX refs_source ON refs(source_block_id);
CREATE INDEX refs_target ON refs(target_id);
CREATE INDEX table_rows_block ON table_rows(block_id, row_index);
"#;
