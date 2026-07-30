use crate::domain::{
    block::{BlockKind, ContentClass, SourceBlock},
    document::ArchiveDocument,
    source::SourceSpan,
};

use super::{
    IngestError, IngestWarning, ParsedDocument, SectionNode,
    diagrams::extract_diagram,
    headings::HeadingBuilder,
    normalize::{
        HeadingDisposition, classify_body, classify_heading, extract_printed_page, is_page_marker,
        normalize_markdown,
    },
    references::{extract_references, resolve_document_references},
    tables::extract_table,
};

#[derive(Clone, Debug)]
struct Line {
    start: usize,
    content_end: usize,
    end: usize,
    number: u32,
}

#[derive(Debug)]
struct LineIndex<'source> {
    source: &'source str,
    lines: Vec<Line>,
}

#[derive(Clone, Debug)]
struct PageMarker {
    byte_start: u64,
    page: String,
}

pub fn parse_document(document: &ArchiveDocument) -> Result<ParsedDocument, IngestError> {
    let index = LineIndex::new(&document.source);
    let front_section_id = format!(
        "front:{}",
        blake3::hash(document.meta.document_id.as_bytes()).to_hex()
    );
    let mut headings = HeadingBuilder::default();
    let mut sections = Vec::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut page_markers = Vec::new();
    let mut known_headings = Vec::new();
    let mut inherited_class = ContentClass::FrontMatter;
    let mut line_index = 0_usize;

    while line_index < index.lines.len() {
        if index.text(line_index).trim().is_empty() {
            line_index += 1;
            continue;
        }

        if let Some((source_level, heading)) = parse_atx_heading(index.text(line_index)) {
            let span = index.span(line_index, line_index);
            let disposition = classify_heading(&heading, sections.is_empty());
            let (section_id, heading_path, content_class) = match disposition {
                HeadingDisposition::Section => {
                    inherited_class = ContentClass::Substantive;
                    let section = headings.push(
                        &document.meta.document_id,
                        source_level,
                        heading.clone(),
                        span.clone(),
                    );
                    let section_id = section.section_id.clone();
                    let heading_path = section.heading_path.clone();
                    known_headings.push(heading.clone());
                    sections.push(section);
                    (section_id, heading_path, ContentClass::Substantive)
                }
                HeadingDisposition::Metadata(class) => {
                    inherited_class = class;
                    let (section_id, heading_path) = current_section(&headings, &front_section_id);
                    (section_id, heading_path, class)
                }
            };
            blocks.push(make_block(
                document,
                &index,
                line_index,
                line_index,
                BlockKind::Prose,
                content_class,
                section_id,
                heading_path,
            ));
            line_index += 1;
            continue;
        }

        if let Some((marker, width)) = fence_marker(index.text(line_index)) {
            let start = line_index;
            let mut end = start;
            let mut closed = false;
            while end + 1 < index.lines.len() {
                end += 1;
                if closes_fence(index.text(end), marker, width) {
                    closed = true;
                    break;
                }
            }
            if !closed {
                warnings.push(IngestWarning {
                    code: "unclosed_fence".into(),
                    message: "fenced block reaches end of document".into(),
                    span: index.span(start, end),
                });
            }
            let info = index.text(start).trim_start_matches(marker).trim();
            let kind = if info
                .split_whitespace()
                .next()
                .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"))
            {
                BlockKind::Diagram
            } else {
                BlockKind::Code
            };
            let (section_id, heading_path) = current_section(&headings, &front_section_id);
            blocks.push(make_block(
                document,
                &index,
                start,
                end,
                kind,
                inherited_class,
                section_id,
                heading_path,
            ));
            line_index = end + 1;
            continue;
        }

        if index
            .text(line_index)
            .to_ascii_lowercase()
            .contains("<page_number>")
        {
            let start = line_index;
            let mut end = start;
            let same_line_closed = index
                .text(start)
                .to_ascii_lowercase()
                .contains("</page_number>");
            let mut closed = same_line_closed;
            while !closed && end + 1 < index.lines.len() {
                end += 1;
                closed = index
                    .text(end)
                    .to_ascii_lowercase()
                    .contains("</page_number>");
            }
            if !closed {
                warnings.push(IngestWarning {
                    code: "unclosed_page_number".into(),
                    message: "page_number element reaches end of document".into(),
                    span: index.span(start, end),
                });
            }
            let raw = index.raw(start, end);
            if let Some(page) = extract_printed_page(raw) {
                page_markers.push(PageMarker {
                    byte_start: index.lines[start].start as u64,
                    page,
                });
            }
            let (section_id, heading_path) = current_section(&headings, &front_section_id);
            blocks.push(make_block(
                document,
                &index,
                start,
                end,
                BlockKind::Prose,
                ContentClass::PageFurniture,
                section_id,
                heading_path,
            ));
            line_index = end + 1;
            continue;
        }

        if is_page_marker(index.text(line_index)) {
            let raw = index.raw(line_index, line_index);
            if let Some(page) = extract_printed_page(raw) {
                page_markers.push(PageMarker {
                    byte_start: index.lines[line_index].start as u64,
                    page,
                });
            }
            let (section_id, heading_path) = current_section(&headings, &front_section_id);
            blocks.push(make_block(
                document,
                &index,
                line_index,
                line_index,
                BlockKind::Prose,
                ContentClass::PageFurniture,
                section_id,
                heading_path,
            ));
            line_index += 1;
            continue;
        }

        if index
            .text(line_index)
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<table")
        {
            let start = line_index;
            let mut end = start;
            let mut closed = index.text(start).to_ascii_lowercase().contains("</table>");
            while !closed && end + 1 < index.lines.len() {
                end += 1;
                closed = index.text(end).to_ascii_lowercase().contains("</table>");
            }
            if !closed {
                warnings.push(IngestWarning {
                    code: "unclosed_html_table".into(),
                    message: "HTML table reaches end of document".into(),
                    span: index.span(start, end),
                });
            }
            push_body_block(
                document,
                &index,
                &headings,
                &front_section_id,
                &known_headings,
                inherited_class,
                &mut blocks,
                start,
                end,
                BlockKind::Table,
            );
            line_index = end + 1;
            continue;
        }

        if is_markdown_table_start(&index, line_index) {
            let start = line_index;
            let mut end = start + 1;
            while end + 1 < index.lines.len()
                && !index.text(end + 1).trim().is_empty()
                && index.text(end + 1).contains('|')
            {
                end += 1;
            }
            push_body_block(
                document,
                &index,
                &headings,
                &front_section_id,
                &known_headings,
                inherited_class,
                &mut blocks,
                start,
                end,
                BlockKind::Table,
            );
            line_index = end + 1;
            continue;
        }

        let start = line_index;
        let kind = if is_list_start(index.text(start)) {
            BlockKind::List
        } else if index.text(start).trim_start().starts_with('>') {
            BlockKind::Quote
        } else if is_caption(index.text(start)) {
            BlockKind::Caption
        } else {
            BlockKind::Prose
        };
        let mut end = start;
        while end + 1 < index.lines.len()
            && !index.text(end + 1).trim().is_empty()
            && !is_block_boundary(&index, end + 1)
        {
            end += 1;
        }
        push_body_block(
            document,
            &index,
            &headings,
            &front_section_id,
            &known_headings,
            inherited_class,
            &mut blocks,
            start,
            end,
            kind,
        );
        line_index = end + 1;
    }

    finalize_sections(&mut sections, &index, &page_markers);
    for block in &mut blocks {
        block.span.printed_page = page_for(block.span.byte_start, &page_markers);
    }

    let mut tables = Vec::new();
    let mut diagrams = Vec::new();
    let mut references = Vec::new();
    for (position, block) in blocks.iter().enumerate() {
        references.extend(extract_references(block));
        match block.kind {
            BlockKind::Table => match extract_table(block) {
                Ok(mut table) => {
                    table.caption = nearest_caption(&blocks, position, "Table ");
                    tables.push(table);
                }
                Err(error) => warnings.push(IngestWarning {
                    code: "table_extraction".into(),
                    message: error.to_string(),
                    span: block.span.clone(),
                }),
            },
            BlockKind::Diagram => {
                let caption = nearest_caption(&blocks, position, "Figure ");
                match extract_diagram(block, caption.as_deref()) {
                    Ok(diagram) => diagrams.push(diagram),
                    Err(error) => warnings.push(IngestWarning {
                        code: "diagram_extraction".into(),
                        message: error.to_string(),
                        span: block.span.clone(),
                    }),
                }
            }
            _ => {}
        }
    }
    resolve_document_references(
        &document.meta.document_id,
        &sections,
        &tables,
        &diagrams,
        &mut references,
    );

    Ok(ParsedDocument {
        document_id: document.meta.document_id.clone(),
        sections,
        blocks,
        tables,
        diagrams,
        references,
        warnings,
    })
}

fn nearest_caption(blocks: &[SourceBlock], position: usize, prefix: &str) -> Option<String> {
    let section_id = &blocks[position].section_id;
    blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            block.kind == BlockKind::Caption
                && block.section_id == *section_id
                && block.normalized_text.starts_with(prefix)
        })
        .min_by_key(|(candidate, _)| candidate.abs_diff(position))
        .map(|(_, block)| block.normalized_text.clone())
}

#[allow(clippy::too_many_arguments)]
fn push_body_block(
    document: &ArchiveDocument,
    index: &LineIndex<'_>,
    headings: &HeadingBuilder,
    front_section_id: &str,
    known_headings: &[String],
    inherited_class: ContentClass,
    blocks: &mut Vec<SourceBlock>,
    start: usize,
    end: usize,
    kind: BlockKind,
) {
    let (section_id, heading_path) = current_section(headings, front_section_id);
    let raw = index.raw(start, end);
    let normalized = normalize_markdown(raw);
    let content_class = classify_body(&normalized, inherited_class, known_headings);
    blocks.push(make_block_with_text(
        document,
        index,
        start,
        end,
        kind,
        content_class,
        section_id,
        heading_path,
        normalized,
    ));
}

#[allow(clippy::too_many_arguments)]
fn make_block(
    document: &ArchiveDocument,
    index: &LineIndex<'_>,
    start: usize,
    end: usize,
    kind: BlockKind,
    content_class: ContentClass,
    section_id: String,
    heading_path: Vec<String>,
) -> SourceBlock {
    let normalized = normalize_markdown(index.raw(start, end));
    make_block_with_text(
        document,
        index,
        start,
        end,
        kind,
        content_class,
        section_id,
        heading_path,
        normalized,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_block_with_text(
    document: &ArchiveDocument,
    index: &LineIndex<'_>,
    start: usize,
    end: usize,
    kind: BlockKind,
    content_class: ContentClass,
    section_id: String,
    heading_path: Vec<String>,
    normalized_text: String,
) -> SourceBlock {
    let span = index.span(start, end);
    let identity = format!(
        "{}\0{}\0{}\0{:?}",
        document.meta.document_id, span.byte_start, span.byte_end, kind
    );
    SourceBlock {
        block_id: format!("blk:{}", blake3::hash(identity.as_bytes()).to_hex()),
        document_id: document.meta.document_id.clone(),
        section_id,
        kind,
        heading_path,
        raw_source: index.raw(start, end).to_owned(),
        normalized_text,
        content_class,
        span,
    }
}

fn current_section(headings: &HeadingBuilder, front_section_id: &str) -> (String, Vec<String>) {
    headings
        .current()
        .map(|(section_id, path)| (section_id.to_owned(), path))
        .unwrap_or_else(|| (front_section_id.to_owned(), Vec::new()))
}

fn finalize_sections(
    sections: &mut [SectionNode],
    index: &LineIndex<'_>,
    page_markers: &[PageMarker],
) {
    for position in 0..sections.len() {
        let level = sections[position].level;
        let end = sections[position + 1..]
            .iter()
            .find(|section| section.level <= level)
            .map_or(index.source.len(), |section| {
                section.span.byte_start as usize
            });
        sections[position].span.byte_end = end as u64;
        sections[position].span.line_end = index.line_for_offset(end.saturating_sub(1));
        sections[position].printed_page =
            page_for(sections[position].span.byte_start, page_markers);
        sections[position].span.printed_page = sections[position].printed_page.clone();
    }
}

fn page_for(byte_start: u64, markers: &[PageMarker]) -> Option<String> {
    markers
        .iter()
        .rev()
        .find(|marker| marker.byte_start <= byte_start)
        .or_else(|| markers.first())
        .map(|marker| marker.page.clone())
}

fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes)
        || !trimmed
            .as_bytes()
            .get(hashes)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let heading = trimmed[hashes..]
        .trim()
        .trim_end_matches('#')
        .trim()
        .to_owned();
    (!heading.is_empty()).then_some((hashes as u8, heading))
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let width = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (width >= 3).then_some((marker, width))
}

fn closes_fence(line: &str, marker: char, width: usize) -> bool {
    let trimmed = line.trim();
    let found = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    found >= width && trimmed[found..].trim().is_empty()
}

fn is_markdown_table_start(index: &LineIndex<'_>, line: usize) -> bool {
    line + 1 < index.lines.len()
        && index.text(line).contains('|')
        && is_markdown_table_separator(index.text(line + 1))
}

fn is_markdown_table_separator(line: &str) -> bool {
    let cells = line.trim().trim_matches('|').split('|').collect::<Vec<_>>();
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim().trim_matches(':');
            cell.len() >= 3 && cell.bytes().all(|byte| byte == b'-')
        })
}

fn is_list_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    if ["- ", "* ", "+ "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && trimmed
            .as_bytes()
            .get(digits..digits + 2)
            .is_some_and(|suffix| suffix == b". " || suffix == b") ")
}

fn is_caption(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["Figure ", "Table ", "Рисунок ", "Таблица "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

fn is_block_boundary(index: &LineIndex<'_>, line: usize) -> bool {
    let text = index.text(line);
    parse_atx_heading(text).is_some()
        || fence_marker(text).is_some()
        || text.to_ascii_lowercase().contains("<page_number>")
        || text.trim_start().to_ascii_lowercase().starts_with("<table")
        || is_page_marker(text)
        || is_markdown_table_start(index, line)
        || is_list_start(text)
        || text.trim_start().starts_with('>')
        || is_caption(text)
}

impl<'source> LineIndex<'source> {
    fn new(source: &'source str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0_usize;
        let mut number = 1_u32;
        for (position, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                let content_end = if position > start && source.as_bytes()[position - 1] == b'\r' {
                    position - 1
                } else {
                    position
                };
                lines.push(Line {
                    start,
                    content_end,
                    end: position + 1,
                    number,
                });
                start = position + 1;
                number += 1;
            }
        }
        if start < source.len() {
            let content_end = source.len();
            lines.push(Line {
                start,
                content_end,
                end: content_end,
                number,
            });
        }
        Self { source, lines }
    }

    fn text(&self, line: usize) -> &'source str {
        let line = &self.lines[line];
        &self.source[line.start..line.content_end]
    }

    fn raw(&self, start: usize, end: usize) -> &'source str {
        &self.source[self.lines[start].start..self.lines[end].end]
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        SourceSpan {
            byte_start: self.lines[start].start as u64,
            byte_end: self.lines[end].end as u64,
            line_start: self.lines[start].number,
            line_end: self.lines[end].number,
            printed_page: None,
        }
    }

    fn line_for_offset(&self, offset: usize) -> u32 {
        self.lines
            .partition_point(|line| line.start <= offset)
            .checked_sub(1)
            .and_then(|line| self.lines.get(line))
            .map_or(1, |line| line.number)
    }
}
