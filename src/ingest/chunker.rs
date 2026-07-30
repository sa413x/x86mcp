use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::LazyLock,
};

use anyhow::{Context, Result};
use regex::Regex;

use crate::domain::{
    block::{BlockKind, ContentClass, SourceBlock},
    chunk::{ChunkKind, SearchChunk},
    document::ArchiveDocument,
    source::SourceSpan,
};

use super::{ExtractedDiagram, ExtractedTable, ParsedDocument};

pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> Result<usize>;
}

#[derive(Clone, Copy, Debug)]
pub struct ChunkConfig {
    pub target_tokens: usize,
    pub overlap_tokens: usize,
    pub table_rows_per_chunk: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_tokens: 384,
            overlap_tokens: 48,
            table_rows_per_chunk: 24,
        }
    }
}

#[derive(Debug)]
struct ProseUnit<'block> {
    block: &'block SourceBlock,
    text: String,
    complete_block: bool,
}

struct ChunkBuilder<'document, 'counter> {
    document: &'document ArchiveDocument,
    counter: &'counter dyn TokenCounter,
    config: ChunkConfig,
    chunks: Vec<SearchChunk>,
    ordinal: u64,
}

pub fn chunk_document(
    document: &ArchiveDocument,
    parsed: &ParsedDocument,
    counter: &dyn TokenCounter,
    config: ChunkConfig,
) -> Result<Vec<SearchChunk>> {
    anyhow::ensure!(config.target_tokens > 0, "target_tokens must be positive");
    anyhow::ensure!(
        config.table_rows_per_chunk > 0,
        "table_rows_per_chunk must be positive"
    );

    let tables = parsed
        .tables
        .iter()
        .map(|table| (table.source_block_id.as_str(), table))
        .collect::<HashMap<_, _>>();
    let diagrams = parsed
        .diagrams
        .iter()
        .map(|diagram| (diagram.source_block_id.as_str(), diagram))
        .collect::<HashMap<_, _>>();
    let mut builder = ChunkBuilder {
        document,
        counter,
        config,
        chunks: Vec::new(),
        ordinal: 0,
    };

    let mut position = 0_usize;
    while position < parsed.blocks.len() {
        let block = &parsed.blocks[position];
        match block.kind {
            BlockKind::Prose | BlockKind::List | BlockKind::Quote | BlockKind::Caption => {
                let start = position;
                position += 1;
                while position < parsed.blocks.len()
                    && is_prose_kind(parsed.blocks[position].kind)
                    && parsed.blocks[position].section_id == block.section_id
                    && parsed.blocks[position].content_class == block.content_class
                {
                    position += 1;
                }
                builder.push_prose_run(&parsed.blocks[start..position])?;
            }
            BlockKind::Table => {
                builder.push_table(block, tables.get(block.block_id.as_str()).copied())?;
                position += 1;
            }
            BlockKind::Diagram => {
                builder.push_diagram(block, diagrams.get(block.block_id.as_str()).copied())?;
                position += 1;
            }
            BlockKind::Code => {
                builder.push_code(block)?;
                position += 1;
            }
        }
    }
    Ok(builder.chunks)
}

impl ChunkBuilder<'_, '_> {
    fn push_prose_run(&mut self, blocks: &[SourceBlock]) -> Result<()> {
        let mut units = Vec::new();
        for block in blocks {
            let prefix = self.prefix(block, None);
            let complete = self.compose(&prefix, &[block.normalized_text.as_str()]);
            if self.counter.count(&complete)? <= self.config.target_tokens {
                units.push(ProseUnit {
                    block,
                    text: block.normalized_text.clone(),
                    complete_block: true,
                });
            } else {
                let parts = split_words_to_budget(
                    &prefix,
                    &block.normalized_text,
                    self.config.target_tokens,
                    self.counter,
                )?;
                units.extend(parts.into_iter().map(|text| ProseUnit {
                    block,
                    text,
                    complete_block: false,
                }));
            }
        }

        let mut pending = Vec::<usize>::new();
        for unit_index in 0..units.len() {
            if !pending.is_empty()
                && self.count_units(&units, &pending, Some(unit_index))? > self.config.target_tokens
            {
                self.emit_units(&units, &pending)?;
                pending =
                    trailing_overlap(&units, &pending, self.config.overlap_tokens, self.counter)?;
                while !pending.is_empty()
                    && self.count_units(&units, &pending, Some(unit_index))?
                        > self.config.target_tokens
                {
                    pending.remove(0);
                }
            }
            pending.push(unit_index);
        }
        if !pending.is_empty() {
            self.emit_units(&units, &pending)?;
        }
        Ok(())
    }

    fn count_units(
        &self,
        units: &[ProseUnit<'_>],
        selected: &[usize],
        extra: Option<usize>,
    ) -> Result<usize> {
        let first = selected
            .first()
            .copied()
            .or(extra)
            .context("counting an empty prose selection")?;
        let prefix = self.prefix(units[first].block, None);
        let texts = selected
            .iter()
            .copied()
            .chain(extra)
            .map(|index| units[index].text.as_str())
            .collect::<Vec<_>>();
        self.counter.count(&self.compose(&prefix, &texts))
    }

    fn emit_units(&mut self, units: &[ProseUnit<'_>], selected: &[usize]) -> Result<()> {
        let first = units[selected[0]].block;
        let prefix = self.prefix(first, None);
        let payload = selected
            .iter()
            .map(|index| units[*index].text.as_str())
            .collect::<Vec<_>>();
        let text = self.compose(&prefix, &payload);
        let source_blocks = selected
            .iter()
            .map(|index| units[*index].block)
            .collect::<Vec<_>>();
        let kind = if source_blocks
            .iter()
            .all(|block| block.kind == BlockKind::List)
        {
            ChunkKind::List
        } else {
            ChunkKind::Prose
        };
        self.emit(kind, first.content_class, &source_blocks, text)
    }

    fn push_table(&mut self, block: &SourceBlock, table: Option<&ExtractedTable>) -> Result<()> {
        let Some(table) = table else {
            return self.emit(
                ChunkKind::Table,
                block.content_class,
                &[block],
                self.compose(&self.prefix(block, None), &[&block.normalized_text]),
            );
        };
        let caption = table.caption.as_deref();
        let prefix = self.prefix(block, caption);
        if table.rows.len() <= self.config.table_rows_per_chunk {
            let payload = render_table(&table.headers, &table.rows);
            let text = self.compose(&prefix, &[&payload]);
            if self.counter.count(&text)? <= self.config.target_tokens {
                return self.emit(ChunkKind::Table, block.content_class, &[block], text);
            }
        }
        let mut start = 0_usize;
        while start < table.rows.len() {
            let mut end = start;
            let mut selected_text = None;
            while end < table.rows.len() && end - start < self.config.table_rows_per_chunk {
                let candidate_end = end + 1;
                let payload = render_table(&table.headers, &table.rows[start..candidate_end]);
                let text = self.compose(&prefix, &[&payload]);
                let exceeds_budget = self.counter.count(&text)? > self.config.target_tokens;
                if exceeds_budget && selected_text.is_some() {
                    break;
                }
                end = candidate_end;
                selected_text = Some(text);
                if exceeds_budget {
                    break;
                }
            }
            self.emit(
                ChunkKind::Table,
                block.content_class,
                &[block],
                selected_text.context("selecting a table row group")?,
            )?;
            start = end;
        }
        Ok(())
    }

    fn push_diagram(
        &mut self,
        block: &SourceBlock,
        diagram: Option<&ExtractedDiagram>,
    ) -> Result<()> {
        let caption = diagram.and_then(|diagram| diagram.caption.as_deref());
        let prefix = self.prefix(block, caption);
        let payload = diagram.map_or_else(
            || block.normalized_text.clone(),
            |diagram| {
                let mut labels = diagram.search_labels.join(" | ");
                if labels.is_empty() {
                    labels = block.normalized_text.clone();
                }
                labels
            },
        );
        self.emit(
            ChunkKind::Diagram,
            block.content_class,
            &[block],
            self.compose(&prefix, &[&payload]),
        )
    }

    fn push_code(&mut self, block: &SourceBlock) -> Result<()> {
        let prefix = self.prefix(block, None);
        let body = strip_code_fence(&block.raw_source);
        let complete = self.compose(&prefix, &[body]);
        if self.counter.count(&complete)? <= self.config.target_tokens {
            return self.emit(ChunkKind::Code, block.content_class, &[block], complete);
        }
        let groups = split_code_groups(body);
        let mut pending = Vec::<&str>::new();
        for group in groups {
            let mut candidate = pending.clone();
            candidate.push(group);
            if !pending.is_empty()
                && self.counter.count(&self.compose(&prefix, &candidate))?
                    > self.config.target_tokens
            {
                let text = self.compose(&prefix, &pending);
                self.emit(ChunkKind::Code, block.content_class, &[block], text)?;
                pending.clear();
            }
            pending.push(group);
        }
        if !pending.is_empty() {
            let text = self.compose(&prefix, &pending);
            self.emit(ChunkKind::Code, block.content_class, &[block], text)?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        kind: ChunkKind,
        content_class: ContentClass,
        source_blocks: &[&SourceBlock],
        text: String,
    ) -> Result<()> {
        let token_count = self.counter.count(&text)?;
        let token_count = u32::try_from(token_count).context("chunk token count exceeds u32")?;
        let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
        let symbols = extract_symbols(&text);
        let source_block_ids = source_blocks
            .iter()
            .map(|block| block.block_id.clone())
            .fold(Vec::<String>::new(), |mut ids, id| {
                if ids.last() != Some(&id) {
                    ids.push(id);
                }
                ids
            });
        let first = source_blocks[0];
        let span = merge_spans(source_blocks);
        let identity = format!(
            "{}\0{:?}\0{}\0{}\0{}",
            self.document.meta.document_id,
            kind,
            self.ordinal,
            source_block_ids.join("\0"),
            content_hash
        );
        self.ordinal += 1;
        self.chunks.push(SearchChunk {
            chunk_id: format!("chk:{}", blake3::hash(identity.as_bytes()).to_hex()),
            document_id: self.document.meta.document_id.clone(),
            section_id: first.section_id.clone(),
            vendor: self.document.meta.vendor,
            heading_path: first.heading_path.clone(),
            source_block_ids,
            kind,
            content_class,
            text,
            token_count,
            content_hash,
            symbols,
            span,
        });
        Ok(())
    }

    fn prefix(&self, block: &SourceBlock, caption: Option<&str>) -> String {
        let title = Path::new(&self.document.meta.entry_path)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&self.document.meta.entry_path);
        let mut parts = Vec::with_capacity(3);
        if !title.is_empty() {
            parts.push(title.to_owned());
        }
        if !block.heading_path.is_empty() {
            parts.push(block.heading_path.join(" > "));
        }
        if let Some(caption) = caption.filter(|caption| !caption.is_empty()) {
            parts.push(caption.to_owned());
        }
        parts.join("\n")
    }

    fn compose(&self, prefix: &str, payloads: &[&str]) -> String {
        let payload_len = payloads.iter().map(|payload| payload.len()).sum::<usize>();
        let mut text = String::with_capacity(prefix.len() + payload_len + payloads.len() * 2);
        if !prefix.is_empty() {
            text.push_str(prefix);
        }
        for payload in payloads {
            if payload.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(payload.trim());
        }
        text
    }
}

fn is_prose_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Prose | BlockKind::List | BlockKind::Quote | BlockKind::Caption
    )
}

fn split_words_to_budget(
    prefix: &str,
    text: &str,
    target_tokens: usize,
    counter: &dyn TokenCounter,
) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut candidate = current.clone();
        if !candidate.is_empty() {
            candidate.push(' ');
        }
        candidate.push_str(word);
        let combined = if prefix.is_empty() {
            candidate.clone()
        } else {
            format!("{prefix}\n\n{candidate}")
        };
        if !current.is_empty() && counter.count(&combined)? > target_tokens {
            parts.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() && !text.is_empty() {
        parts.push(text.to_owned());
    }
    Ok(parts)
}

fn trailing_overlap(
    units: &[ProseUnit<'_>],
    selected: &[usize],
    budget: usize,
    counter: &dyn TokenCounter,
) -> Result<Vec<usize>> {
    if budget == 0 {
        return Ok(Vec::new());
    }
    let mut overlap = Vec::new();
    let mut text = String::new();
    for index in selected.iter().rev().copied() {
        if !units[index].complete_block {
            break;
        }
        let candidate = if text.is_empty() {
            units[index].text.clone()
        } else {
            format!("{}\n\n{text}", units[index].text)
        };
        if counter.count(&candidate)? > budget {
            break;
        }
        text = candidate;
        overlap.push(index);
    }
    overlap.reverse();
    Ok(overlap)
}

fn render_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut text = headers.join(" | ");
    for row in rows {
        text.push('\n');
        text.push_str(&row.join(" | "));
    }
    text
}

fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") && !trimmed.starts_with("~~~") {
        return trimmed;
    }
    let body = trimmed.split_once('\n').map_or("", |(_, body)| body);
    body.strip_suffix("```")
        .or_else(|| body.strip_suffix("~~~"))
        .unwrap_or(body)
        .trim()
}

fn split_code_groups(code: &str) -> Vec<&str> {
    static BLANK_LINES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\r?\n[ \t]*\r?\n+").expect("blank-line regex must compile"));
    BLANK_LINES
        .split(code)
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .collect()
}

fn merge_spans(blocks: &[&SourceBlock]) -> SourceSpan {
    let first = &blocks[0].span;
    let last = &blocks[blocks.len() - 1].span;
    SourceSpan {
        byte_start: first.byte_start,
        byte_end: last.byte_end,
        line_start: first.line_start,
        line_end: last.line_end,
        printed_page: first.printed_page.clone(),
    }
}

fn extract_symbols(text: &str) -> Vec<String> {
    static SYMBOLS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?x)
            \#(?:UD|GP|PF|SS|AC|NM|DB|BP)
            | IA32_[A-Z0-9_]+
            | CPUID(?:\.[A-Z0-9_:,.-]+)?
            | (?:CR|DR)\d(?:\.[A-Z0-9_]+)?
            | [A-Z][A-Z0-9_]{2,}(?:\.[A-Z0-9_]+)*
            ",
        )
        .expect("x86 symbol regex must compile")
    });
    let mut seen = HashSet::new();
    SYMBOLS
        .find_iter(text)
        .map(|matched| matched.as_str().to_owned())
        .filter(|symbol| seen.insert(symbol.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::{
        domain::{
            Vendor,
            chunk::ChunkKind,
            document::{ArchiveDocument, DocumentMeta},
        },
        ingest::parse_document,
    };

    use super::{ChunkConfig, TokenCounter, chunk_document};

    struct WhitespaceCounter;

    impl TokenCounter for WhitespaceCounter {
        fn count(&self, text: &str) -> Result<usize> {
            Ok(text.split_whitespace().count())
        }
    }

    fn document(source: &str) -> ArchiveDocument {
        ArchiveDocument {
            meta: DocumentMeta {
                document_id: "doc:chunks".into(),
                vendor: Vendor::Intel,
                archive_id: "intel".into(),
                archive_sha256: "0".repeat(64),
                entry_path: "manual.md".into(),
                content_sha256: "1".repeat(64),
                byte_len: source.len() as u64,
            },
            source: source.into(),
        }
    }

    #[test]
    fn prose_is_bounded_and_overlap_only_reuses_adjacent_blocks() {
        let document = document("one two\n\nthree four\n\nfive six\n\nseven eight\n");
        let parsed = parse_document(&document).unwrap();
        let chunks = chunk_document(
            &document,
            &parsed,
            &WhitespaceCounter,
            ChunkConfig {
                target_tokens: 5,
                overlap_tokens: 2,
                ..ChunkConfig::default()
            },
        )
        .unwrap();
        assert!(chunks.iter().all(|chunk| chunk.token_count <= 5));
        assert!(chunks.windows(2).all(|pair| {
            let shared = pair[0]
                .source_block_ids
                .iter()
                .filter(|id| pair[1].source_block_ids.contains(id))
                .count();
            shared <= 1
        }));
        assert!(chunks.windows(2).any(|pair| {
            pair[0]
                .source_block_ids
                .iter()
                .any(|id| pair[1].source_block_ids.contains(id))
        }));
    }

    #[test]
    fn small_structural_blocks_remain_indivisible() {
        let document = document(
            "```text\nMOV EAX, CR4\n```\n\n| Bit | Meaning |\n| --- | --- |\n| 13 | VMXE |\n\n```mermaid\ngraph TD\nA --> B\n```\n",
        );
        let parsed = parse_document(&document).unwrap();
        let chunks = chunk_document(
            &document,
            &parsed,
            &WhitespaceCounter,
            ChunkConfig {
                target_tokens: 64,
                ..ChunkConfig::default()
            },
        )
        .unwrap();
        for kind in [ChunkKind::Code, ChunkKind::Table, ChunkKind::Diagram] {
            let chunk = chunks.iter().find(|chunk| chunk.kind == kind).unwrap();
            assert_eq!(chunk.source_block_ids.len(), 1);
        }
    }

    #[test]
    fn large_table_chunks_repeat_headers_by_row_group() {
        let document = document(
            "| Bit | Meaning |\n| --- | --- |\n| 0 | Zero |\n| 1 | One |\n| 2 | Two |\n| 3 | Three |\n| 4 | Four |\n",
        );
        let parsed = parse_document(&document).unwrap();
        let chunks = chunk_document(
            &document,
            &parsed,
            &WhitespaceCounter,
            ChunkConfig {
                target_tokens: 7,
                table_rows_per_chunk: 2,
                ..ChunkConfig::default()
            },
        )
        .unwrap();
        let tables = chunks
            .iter()
            .filter(|chunk| chunk.kind == ChunkKind::Table)
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 5);
        assert!(
            tables
                .iter()
                .all(|chunk| chunk.text.contains("Bit | Meaning"))
        );
        assert!(tables.iter().all(|chunk| chunk.token_count <= 7));
    }

    #[test]
    fn large_code_splits_only_at_blank_lines() {
        let document = document(
            "```text\nMOV EAX CR4\nSET BIT THIRTEEN\n\nREAD IA32 FEATURE CONTROL\nLOCK THE REGISTER\n```\n",
        );
        let parsed = parse_document(&document).unwrap();
        let chunks = chunk_document(
            &document,
            &parsed,
            &WhitespaceCounter,
            ChunkConfig {
                target_tokens: 8,
                ..ChunkConfig::default()
            },
        )
        .unwrap();
        let code = chunks
            .iter()
            .filter(|chunk| chunk.kind == ChunkKind::Code)
            .collect::<Vec<_>>();
        assert_eq!(code.len(), 2);
        assert!(code[0].text.contains("SET BIT THIRTEEN"));
        assert!(!code[0].text.contains("READ IA32"));
        assert!(code[1].text.contains("READ IA32 FEATURE CONTROL"));
    }

    #[test]
    fn chunk_ids_are_deterministic_and_content_sensitive() {
        let config = ChunkConfig::default();
        let first_document = document("VMX uses CR4.VMXE.\n");
        let first_parsed = parse_document(&first_document).unwrap();
        let first =
            chunk_document(&first_document, &first_parsed, &WhitespaceCounter, config).unwrap();
        let repeated =
            chunk_document(&first_document, &first_parsed, &WhitespaceCounter, config).unwrap();
        assert_eq!(first[0].chunk_id, repeated[0].chunk_id);

        let changed_document = document("VMX uses CR4.VMXE and VMXON.\n");
        let changed_parsed = parse_document(&changed_document).unwrap();
        let changed = chunk_document(
            &changed_document,
            &changed_parsed,
            &WhitespaceCounter,
            config,
        )
        .unwrap();
        assert_ne!(first[0].chunk_id, changed[0].chunk_id);
    }
}
