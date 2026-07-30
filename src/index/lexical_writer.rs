use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use tantivy::{Index, TantivyDocument};

use crate::domain::{
    block::ContentClass,
    chunk::{ChunkKind, SearchChunk},
};

use super::{
    IndexError, LexicalBuildStats,
    schema::{LexicalFields, build_schema},
    tokenizer::{normalize_symbol, register},
};

const WRITER_MEMORY_BYTES: usize = 128 * 1024 * 1024;

pub struct LexicalWriter {
    index: Index,
    fields: LexicalFields,
    path: PathBuf,
}

impl LexicalWriter {
    pub fn create(path: &Path) -> Result<Self, IndexError> {
        if path.exists() && fs::read_dir(path)?.next().is_some() {
            return Err(IndexError::InvalidRequest(format!(
                "lexical directory is not empty: {}",
                path.display()
            )));
        }
        fs::create_dir_all(path)?;
        let (schema, fields) = build_schema();
        let index = Index::create_in_dir(path, schema)?;
        register(&index)?;
        Ok(Self {
            index,
            fields,
            path: path.to_path_buf(),
        })
    }

    pub fn write(self, chunks: &[SearchChunk]) -> Result<LexicalBuildStats, IndexError> {
        let mut ordered = chunks.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        if let Some(duplicate) = ordered
            .windows(2)
            .find(|pair| pair[0].chunk_id == pair[1].chunk_id)
        {
            return Err(IndexError::DuplicateChunkId(duplicate[0].chunk_id.clone()));
        }

        let mut writer = self.index.writer::<TantivyDocument>(WRITER_MEMORY_BYTES)?;
        for chunk in ordered {
            writer.add_document(self.document(chunk))?;
        }
        writer.commit()?;
        writer.wait_merging_threads()?;

        Ok(LexicalBuildStats {
            document_count: chunks.len() as u64,
            component_checksum: component_checksum(&self.path)?,
        })
    }

    fn document(&self, chunk: &SearchChunk) -> TantivyDocument {
        let mut document = TantivyDocument::new();
        document.add_text(self.fields.chunk_id, &chunk.chunk_id);
        document.add_text(self.fields.vendor, vendor_key(chunk.vendor));
        document.add_text(self.fields.document_id, &chunk.document_id);
        document.add_text(self.fields.kind, kind_key(chunk.kind));
        document.add_text(self.fields.heading, chunk.heading_path.join(" > "));
        if let Some(caption) = structural_caption(chunk) {
            document.add_text(self.fields.caption, caption);
        }
        if chunk.kind == ChunkKind::Code {
            document.add_text(self.fields.code, &chunk.text);
        } else {
            document.add_text(self.fields.body, &chunk.text);
        }
        for symbol in &chunk.symbols {
            let normalized = normalize_symbol(symbol);
            if !normalized.is_empty() {
                document.add_text(self.fields.symbol, normalized);
            }
        }
        document.add_f64(
            self.fields.front_matter_weight,
            if chunk.content_class == ContentClass::FrontMatter {
                0.35
            } else {
                1.0
            },
        );
        if let Some(printed_page) = &chunk.span.printed_page {
            document.add_text(self.fields.printed_page, printed_page);
        }
        document.add_u64(self.fields.byte_start, chunk.span.byte_start);
        document.add_u64(self.fields.byte_end, chunk.span.byte_end);
        document.add_u64(self.fields.line_start, u64::from(chunk.span.line_start));
        document.add_u64(self.fields.line_end, u64::from(chunk.span.line_end));
        document
    }
}

pub(crate) fn vendor_key(vendor: crate::domain::Vendor) -> &'static str {
    match vendor {
        crate::domain::Vendor::Intel => "intel",
        crate::domain::Vendor::Amd => "amd",
    }
}

pub(crate) fn kind_key(kind: ChunkKind) -> &'static str {
    match kind {
        ChunkKind::Prose => "prose",
        ChunkKind::List => "list",
        ChunkKind::Code => "code",
        ChunkKind::Table => "table",
        ChunkKind::Diagram => "diagram",
    }
}

fn structural_caption(chunk: &SearchChunk) -> Option<&str> {
    if !matches!(chunk.kind, ChunkKind::Table | ChunkKind::Diagram) {
        return None;
    }
    let prefix = chunk.text.split("\n\n").next()?;
    let mut lines = prefix.lines();
    lines.next()?;
    lines.next()?;
    lines.next_back().or_else(|| prefix.lines().nth(2))
}

pub(crate) fn component_checksum(root: &Path) -> Result<String, IndexError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    for (relative, path) in files {
        let name = relative.as_bytes();
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name);
        let mut reader = BufReader::new(File::open(path)?);
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn collect_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), IndexError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, output)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| IndexError::Corrupt(error.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            output.push((relative, path));
        }
    }
    Ok(())
}
