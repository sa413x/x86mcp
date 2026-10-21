use std::sync::LazyLock;

use schemars::JsonSchema;
use scraper::{ElementRef, Html, Selector};
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

/// One grid position covered by a source cell.
#[derive(Clone, Copy, Debug)]
struct Slot {
    cell: usize,
    first_column: bool,
}

/// The HTML specification's upper bound for `colspan`.
const MAX_COLSPAN: usize = 1000;

pub fn extract_table(block: &SourceBlock) -> Result<ExtractedTable, TableError> {
    if block.kind != BlockKind::Table {
        return Err(TableError::WrongBlockKind {
            block_id: block.block_id.clone(),
        });
    }
    let (mut headers, mut rows, mut warnings) = if block
        .raw_source
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("<table")
    {
        parse_html_table(&block.raw_source)
    } else {
        parse_markdown_table(&block.raw_source)
    };
    if headers.is_empty() && rows.iter().all(Vec::is_empty) {
        return Err(TableError::Empty {
            block_id: block.block_id.clone(),
        });
    }
    normalize_widths(&mut headers, &mut rows, &mut warnings);
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
    static ROWS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("tr").expect("valid selector"));

    let fragment = Html::parse_fragment(raw);
    let rows = fragment.select(&ROWS).collect::<Vec<_>>();
    let in_thead = |row: &ElementRef<'_>| {
        row.ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| ancestor.value().name().eq_ignore_ascii_case("thead"))
    };
    // Explicit `<thead>` rows are headers. Without them, leading rows made only
    // of `<th>` cells are headers (the HTML parser wraps every row in `<tbody>`).
    let header_count = if rows.iter().any(in_thead) {
        rows.iter().take_while(|row| in_thead(row)).count()
    } else {
        rows.iter()
            .take_while(|row| {
                let mut cells = row_cells(row).peekable();
                cells.peek().is_some() && cells.all(|cell| cell.value().name() == "th")
            })
            .count()
    };

    let (texts, grid) = build_grid(&rows, header_count);
    let header_width = grid[..header_count].iter().map(Vec::len).max().unwrap_or(0);
    let headers = (0..header_width)
        .map(|column| header_label(&grid[..header_count], &texts, column))
        .collect();
    let data = grid[header_count..]
        .iter()
        .map(|row| {
            row.iter()
                .map(|slot| match slot {
                    Some(slot) if slot.first_column => texts[slot.cell].clone(),
                    _ => String::new(),
                })
                .collect()
        })
        .collect();
    (headers, data, Vec::new())
}

fn row_cells<'a>(row: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> {
    row.children().filter_map(ElementRef::wrap).filter(|cell| {
        let name = cell.value().name();
        name.eq_ignore_ascii_case("th") || name.eq_ignore_ascii_case("td")
    })
}

/// Places every cell on a row/column grid, honoring `rowspan` and `colspan`.
/// Rowspan continuations repeat the cell; colspan continuations are marked so
/// data rows keep one value per source cell. Header rowspans stop at the end
/// of the header rows, as row groups bound them in HTML.
fn build_grid(
    rows: &[ElementRef<'_>],
    header_count: usize,
) -> (Vec<String>, Vec<Vec<Option<Slot>>>) {
    let mut texts = Vec::new();
    let mut grid = vec![Vec::<Option<Slot>>::new(); rows.len()];
    for (row_index, row) in rows.iter().enumerate() {
        let group_end = if row_index < header_count {
            header_count
        } else {
            rows.len()
        };
        let mut column = 0_usize;
        for cell in row_cells(row) {
            while grid[row_index].get(column).is_some_and(Option::is_some) {
                column += 1;
            }
            let id = texts.len();
            texts.push(cell_text(&cell));
            let colspan = span_value(&cell, "colspan").min(MAX_COLSPAN);
            let rowspan = span_value(&cell, "rowspan").min(group_end - row_index);
            for target in &mut grid[row_index..row_index + rowspan] {
                if target.len() < column + colspan {
                    target.resize(column + colspan, None);
                }
                for offset in 0..colspan {
                    target[column + offset].get_or_insert(Slot {
                        cell: id,
                        first_column: offset == 0,
                    });
                }
            }
            column += colspan;
        }
    }
    (texts, grid)
}

/// Joins the distinct non-empty header cells above a leaf column, e.g.
/// `Typical / Address Size (bits)`.
fn header_label(header_rows: &[Vec<Option<Slot>>], texts: &[String], column: usize) -> String {
    let mut parts = Vec::<&str>::new();
    let mut previous_cell = None;
    for slot in header_rows
        .iter()
        .filter_map(|row| row.get(column).copied().flatten())
    {
        if previous_cell == Some(slot.cell) {
            continue;
        }
        previous_cell = Some(slot.cell);
        let text = texts[slot.cell].as_str();
        if !text.is_empty() && parts.last() != Some(&text) {
            parts.push(text);
        }
    }
    parts.join(" / ")
}

fn span_value(cell: &ElementRef<'_>, attribute: &str) -> usize {
    cell.value()
        .attr(attribute)
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

fn cell_text(cell: &ElementRef<'_>) -> String {
    let html = cell
        .inner_html()
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("<br />", " ");
    let fragment = Html::parse_fragment(&html);
    let text = fragment.root_element().text().collect::<Vec<_>>().join(" ");
    normalize_markdown(&text)
}

/// Gives every row exactly one value per column without dropping data: rows
/// wider than the header extend it with unlabeled columns, shorter rows are
/// padded with empty cells.
fn normalize_widths(
    headers: &mut Vec<String>,
    rows: &mut [Vec<String>],
    warnings: &mut Vec<ExtractionWarning>,
) {
    let header_width = headers.len();
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(header_width);
    if width > header_width {
        if header_width > 0 {
            warnings.push(ExtractionWarning {
                code: "table_header_extended".into(),
                message: format!(
                    "rows have up to {width} cells; header has {header_width}; added {} unlabeled header columns",
                    width - header_width
                ),
            });
        }
        headers.resize(width, String::new());
    }
    for (row_index, row) in rows.iter_mut().enumerate() {
        if row.len() < width {
            warnings.push(ExtractionWarning {
                code: "table_width_mismatch".into(),
                message: format!(
                    "row {} has {} cells; table has {width} columns",
                    row_index + 1,
                    row.len()
                ),
            });
            row.resize(width, String::new());
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
        assert_eq!(
            table.headers,
            vec!["Register / Bit(s)", "Register / Description"]
        );
        assert_eq!(table.rows[0], vec!["13", "VMX Enable"]);
        assert_eq!(table.rows[1], vec!["13", "Lock"]);
        assert!(table.warnings.is_empty());
    }

    #[test]
    fn header_grid_keeps_rowspan_columns_from_earlier_header_rows() {
        let block = table_block(
            "<table><thead><tr><th colspan=\"2\">Mode</th><th rowspan=\"2\">OS</th><th colspan=\"2\">Typical</th></tr><tr><th>Long</th><th>Sub</th><th>Address</th><th>Operand</th></tr></thead><tbody><tr><td>Long Mode</td><td>64-Bit</td><td>64-bit OS</td><td>64</td><td>32</td></tr></tbody></table>\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(
            table.headers,
            vec![
                "Mode / Long",
                "Mode / Sub",
                "OS",
                "Typical / Address",
                "Typical / Operand"
            ]
        );
        assert_eq!(
            table.rows[0],
            vec!["Long Mode", "64-Bit", "64-bit OS", "64", "32"]
        );
        assert!(table.warnings.is_empty());
    }

    #[test]
    fn rows_wider_than_header_extend_it_instead_of_losing_cells() {
        let block = table_block(
            "<table><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td><td>2</td><td>3</td></tr></tbody></table>\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(table.headers, vec!["A", "B", ""]);
        assert_eq!(table.rows[0], vec!["1", "2", "3"]);
        assert!(
            table
                .warnings
                .iter()
                .any(|warning| warning.code == "table_header_extended")
        );
    }

    #[test]
    fn headerless_html_tables_are_kept() {
        let block = table_block(
            "<table><tr><td>JNO</td><td>Jump if not overflow.</td></tr><tr><td>JS</td><td>Jump if sign (negative).</td></tr></table>\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(table.headers, vec!["", ""]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1], vec!["JS", "Jump if sign (negative)."]);
        assert!(table.warnings.is_empty());
    }

    #[test]
    fn inferred_header_row_is_not_repeated_as_data() {
        let block = table_block(
            "<table><tr><th>Legend</th><th>Description</th></tr><tr><td>Bits copied icon</td><td>Bits copied</td></tr></table>\n",
        );
        let table = extract_table(&block).unwrap();
        assert_eq!(table.headers, vec!["Legend", "Description"]);
        assert_eq!(
            table.rows,
            vec![vec![
                "Bits copied icon".to_owned(),
                "Bits copied".to_owned()
            ]]
        );
    }

    #[test]
    fn tables_without_rows_are_rejected() {
        assert!(extract_table(&table_block("<table></table>\n")).is_err());
    }
}
