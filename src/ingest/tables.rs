use std::sync::LazyLock;

use schemars::JsonSchema;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::block::{BlockKind, SourceBlock};

use super::{ExtractionWarning, normalize::normalize_markdown};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ExtractedTable {
    pub table_id: String,
    pub source_block_id: String,
    pub caption: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub raw_source: String,
    pub warnings: Vec<ExtractionWarning>,
}

#[derive(Debug, Error)]
pub enum TableError {
    #[error("source block {block_id} is not a table")]
    WrongBlockKind { block_id: String },
    #[error("source block {block_id} contains no table rows")]
    Empty { block_id: String },
}

#[derive(Clone, Debug)]
struct PendingSpan {
    value: String,
    remaining_rows: usize,
}

pub fn extract_table(block: &SourceBlock) -> Result<ExtractedTable, TableError> {
    if block.kind != BlockKind::Table {
        return Err(TableError::WrongBlockKind {
            block_id: block.block_id.clone(),
        });
    }
    let (headers, mut rows, mut warnings) = if block
        .raw_source
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("<table")
    {
        parse_html_table(&block.raw_source)
    } else {
        parse_markdown_table(&block.raw_source)
    };
    if headers.is_empty() {
        return Err(TableError::Empty {
            block_id: block.block_id.clone(),
        });
    }
    normalize_widths(&headers, &mut rows, &mut warnings);
    let identity = format!("{}\0{}", block.document_id, block.block_id);
    Ok(ExtractedTable {
        table_id: format!("tbl:{}", blake3::hash(identity.as_bytes()).to_hex()),
        source_block_id: block.block_id.clone(),
        caption: None,
        headers,
        rows,
        raw_source: block.raw_source.clone(),
        warnings,
    })
}

fn parse_markdown_table(raw: &str) -> (Vec<String>, Vec<Vec<String>>, Vec<ExtractionWarning>) {
    let mut parsed = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(split_markdown_row)
        .collect::<Vec<_>>();
    if parsed.len() < 2 || !is_separator_row(&parsed[1]) {
        return (
            Vec::new(),
            Vec::new(),
            vec![ExtractionWarning {
                code: "missing_markdown_separator".into(),
                message: "Markdown table has no valid header separator".into(),
            }],
        );
    }
    let headers = parsed
        .remove(0)
        .into_iter()
        .map(|cell| normalize_markdown(&cell))
        .collect();
    parsed.remove(0);
    for row in &mut parsed {
        for cell in row {
            *cell = normalize_markdown(cell);
        }
    }
    (headers, parsed, Vec::new())
}

fn split_markdown_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for character in trimmed.chars() {
        if escaped {
            if character != '|' {
                cell.push('\\');
            }
            cell.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '|' {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(character);
        }
    }
    if escaped {
        cell.push('\\');
    }
    cells.push(cell.trim().to_owned());
    cells
}

fn is_separator_row(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let cell = cell.trim().trim_matches(':');
            cell.len() >= 3 && cell.bytes().all(|byte| byte == b'-')
        })
}

fn parse_html_table(raw: &str) -> (Vec<String>, Vec<Vec<String>>, Vec<ExtractionWarning>) {
    static THEAD_ROWS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("thead tr").expect("valid selector"));
    static TBODY_ROWS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("tbody tr").expect("valid selector"));
    static ALL_ROWS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("tr").expect("valid selector"));
    static CELLS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("th, td").expect("valid selector"));

    let fragment = Html::parse_fragment(raw);
    let header_rows = fragment.select(&THEAD_ROWS).collect::<Vec<_>>();
    let headers = header_rows
        .last()
        .map(|row| expand_simple_row(row, &CELLS))
        .unwrap_or_default();
    let mut data_rows = fragment.select(&TBODY_ROWS).collect::<Vec<_>>();
    if data_rows.is_empty() {
        data_rows = fragment.select(&ALL_ROWS).collect();
        if !header_rows.is_empty() {
            data_rows.retain(|row| !row.value().name().eq_ignore_ascii_case("thead"));
        } else if data_rows
            .first()
            .is_some_and(|row| row.select(&CELLS).any(|cell| cell.value().name() == "th"))
        {
            data_rows.remove(0);
        }
    }
    let headers = if headers.is_empty() {
        fragment
            .select(&ALL_ROWS)
            .find(|row| row.select(&CELLS).any(|cell| cell.value().name() == "th"))
            .map(|row| expand_simple_row(&row, &CELLS))
            .unwrap_or_default()
    } else {
        headers
    };

    let mut pending = Vec::<Option<PendingSpan>>::new();
    let mut rows = Vec::with_capacity(data_rows.len());
    for row in data_rows {
        rows.push(expand_data_row(&row, &CELLS, &mut pending));
    }
    (headers, rows, Vec::new())
}

fn expand_simple_row(row: &scraper::ElementRef<'_>, cells: &Selector) -> Vec<String> {
    let mut output = Vec::new();
    for cell in row.select(cells) {
        let colspan = span_value(&cell, "colspan");
        output.push(cell_text(&cell));
        output.extend((1..colspan).map(|_| String::new()));
    }
    output
}

fn expand_data_row(
    row: &scraper::ElementRef<'_>,
    cells: &Selector,
    pending: &mut Vec<Option<PendingSpan>>,
) -> Vec<String> {
    let mut output = Vec::new();
    let mut column = 0_usize;
    for cell in row.select(cells) {
        consume_pending(pending, &mut output, &mut column);
        let colspan = span_value(&cell, "colspan");
        let rowspan = span_value(&cell, "rowspan");
        let value = cell_text(&cell);
        for offset in 0..colspan {
            let expanded = if offset == 0 {
                value.clone()
            } else {
                String::new()
            };
            output.push(expanded.clone());
            if rowspan > 1 {
                if pending.len() <= column {
                    pending.resize(column + 1, None);
                }
                pending[column] = Some(PendingSpan {
                    value: expanded,
                    remaining_rows: rowspan - 1,
                });
            }
            column += 1;
        }
    }
    consume_pending(pending, &mut output, &mut column);
    output
}

fn consume_pending(
    pending: &mut [Option<PendingSpan>],
    output: &mut Vec<String>,
    column: &mut usize,
) {
    while let Some(Some(span)) = pending.get_mut(*column) {
        output.push(span.value.clone());
        span.remaining_rows -= 1;
        if span.remaining_rows == 0 {
            pending[*column] = None;
        }
        *column += 1;
    }
}

fn span_value(cell: &scraper::ElementRef<'_>, attribute: &str) -> usize {
    cell.value()
        .attr(attribute)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

fn cell_text(cell: &scraper::ElementRef<'_>) -> String {
    let html = cell
        .inner_html()
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("<br />", " ");
    let fragment = Html::parse_fragment(&html);
    let text = fragment.root_element().text().collect::<Vec<_>>().join(" ");
    normalize_markdown(&text)
}

fn normalize_widths(
    headers: &[String],
    rows: &mut [Vec<String>],
    warnings: &mut Vec<ExtractionWarning>,
) {
    for (row_index, row) in rows.iter_mut().enumerate() {
        if row.len() != headers.len() {
            warnings.push(ExtractionWarning {
                code: "table_width_mismatch".into(),
                message: format!(
                    "row {} has {} cells; header has {}",
                    row_index + 1,
                    row.len(),
                    headers.len()
                ),
            });
            row.resize(headers.len(), String::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        block::{BlockKind, ContentClass, SourceBlock},
        source::SourceSpan,
    };

    use super::extract_table;

    fn table_block(raw_source: &str) -> SourceBlock {
        SourceBlock {
            block_id: "blk:table".into(),
            document_id: "doc:test".into(),
            section_id: "sec:test".into(),
            kind: BlockKind::Table,
            heading_path: vec!["CHAPTER 1".into()],
            raw_source: raw_source.into(),
            normalized_text: raw_source.into(),
            content_class: ContentClass::Substantive,
            span: SourceSpan::default(),
        }
    }

    #[test]
    fn parses_markdown_tables_with_escaped_pipes() {
        let block = table_block(
            "| Bit(s) | Description |\n| --- | --- |\n| 13 | VMX Enable |\n| 0 | Lock \\| policy |\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(table.headers, vec!["Bit(s)", "Description"]);
        assert_eq!(table.rows[0], vec!["13", "VMX Enable"]);
        assert_eq!(table.rows[1], vec!["0", "Lock | policy"]);
        assert_eq!(table.raw_source, block.raw_source);
    }

    #[test]
    fn expands_html_colspan_rowspan_and_breaks() {
        let block = table_block(
            "<table><thead><tr><th colspan=\"2\">Register</th></tr><tr><th>Bit(s)</th><th>Description</th></tr></thead><tbody><tr><td rowspan=\"2\">13</td><td>VMX<br>Enable</td></tr><tr><td>Lock</td></tr></tbody></table>\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(table.headers, vec!["Bit(s)", "Description"]);
        assert_eq!(table.rows[0], vec!["13", "VMX Enable"]);
        assert_eq!(table.rows[1], vec!["13", "Lock"]);
        assert!(table.warnings.is_empty());
    }
}
