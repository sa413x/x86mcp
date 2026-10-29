//! MCP resources: the index status, one outline per manual, and a template
//! that renders any section or block as Markdown with its citation.

use std::fmt::Write as _;

use rmcp::{
    ErrorData,
    model::{
        ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
        ResourceContents, ResourceTemplate,
    },
};
use serde_json::json;

use crate::{
    catalog::CatalogDocument,
    domain::{block::ContentClass, citation::Citation},
    query::{GetOutlineRequest, GetSectionRequest, QueryEngine, QueryError},
};

use super::{
    server::X86McpServer,
    types::{join_error, query_error, vendor_label},
};

const STATUS_URI: &str = "x86://index/status";
const DOCUMENT_PREFIX: &str = "x86://documents/";
const SECTION_PREFIX: &str = "x86://sections/";
const SECTION_TEMPLATE: &str = "x86://sections/{id}";
const MARKDOWN: &str = "text/markdown";
const JSON: &str = "application/json";
/// Top-level headings listed in a document resource; `x86_get_outline` pages through the rest.
const OUTLINE_LIMIT: u32 = 300;
/// Block source included in a section resource; `x86_get_section` pages through the rest.
const SECTION_BUDGET_BYTES: usize = 128 * 1024;
/// Blocks fetched per `get_section` page while rendering a section resource.
const SECTION_PAGE_BLOCKS: u32 = 100;

/// The status resource is always listed; manual outlines appear once a usable index exists.
pub(crate) async fn list(server: &X86McpServer) -> Result<ListResourcesResult, ErrorData> {
    let mut resources = vec![
        Resource::new(STATUS_URI, "index-status")
            .with_title("x86 index status")
            .with_description(
                "Readiness, freshness, and counts of the installed Intel SDM/AMD APM index, as reported by x86_index_status.",
            )
            .with_mime_type(JSON),
    ];
    if let Ok(engine) = server.engine().await {
        let documents = tokio::task::spawn_blocking(move || documents(&engine))
            .await
            .map_err(join_error)?
            .map_err(query_error)?;
        resources.extend(documents.iter().map(document_resource));
    }
    Ok(ListResourcesResult::with_all_items(resources))
}

pub(crate) fn templates() -> ListResourceTemplatesResult {
    ListResourceTemplatesResult::with_all_items(vec![
        ResourceTemplate::new(SECTION_TEMPLATE, "section")
            .with_title("x86 manual section")
            .with_description(
                "Markdown text of a manual section or block with its citation and subsections. Use a section_id or block ID from x86_search, x86_lookup, x86_get_outline, or a manual outline resource.",
            )
            .with_mime_type(MARKDOWN),
    ])
}

/// Renders the resource for an ID, or `None` when the ID is unknown.
type Render = fn(&QueryEngine, &str) -> Result<Option<String>, QueryError>;

pub(crate) async fn read(
    server: &X86McpServer,
    uri: &str,
) -> Result<ReadResourceResult, ErrorData> {
    if uri == STATUS_URI {
        let status = server.index_status().await?;
        let text = serde_json::to_string_pretty(&status)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        return Ok(contents(uri, text, JSON));
    }
    let (id, render): (_, Render) = if let Some(id) = uri.strip_prefix(DOCUMENT_PREFIX) {
        (id, document_outline)
    } else if let Some(id) = uri.strip_prefix(SECTION_PREFIX) {
        (id, section_text)
    } else {
        return Err(not_found(uri));
    };
    let id = percent_decode(id).ok_or_else(|| not_found(uri))?;
    match server.execute(move |engine| render(engine, &id)).await? {
        Some(text) => Ok(contents(uri, text, MARKDOWN)),
        None => Err(not_found(uri)),
    }
}

fn contents(uri: &str, text: String, mime_type: &str) -> ReadResourceResult {
    ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type(mime_type),
    ])
}

fn not_found(uri: &str) -> ErrorData {
    ErrorData::resource_not_found(
        format!("no x86 manual resource at {uri}"),
        Some(json!({ "uri": uri })),
    )
}

fn documents(engine: &QueryEngine) -> Result<Vec<CatalogDocument>, QueryError> {
    Ok(engine
        .get_outline(GetOutlineRequest {
            document_id: None,
            root_section_id: None,
            depth: 0,
            limit: 500,
            cursor: None,
        })?
        .documents)
}

fn document_resource(document: &CatalogDocument) -> Resource {
    let meta = &document.meta;
    let name = meta
        .entry_path
        .strip_suffix(".md")
        .unwrap_or(&meta.entry_path);
    Resource::new(format!("{DOCUMENT_PREFIX}{}", meta.document_id), name)
        .with_title(format!(
            "{} manual outline: {name}",
            vendor_label(meta.vendor)
        ))
        .with_description(format!(
            "Top-level section outline of {} with section IDs for x86://sections/{{id}}.",
            meta.entry_path
        ))
        .with_mime_type(MARKDOWN)
}

fn document_outline(engine: &QueryEngine, document_id: &str) -> Result<Option<String>, QueryError> {
    let Some(document) = engine.snapshot.catalog.document(document_id)? else {
        return Ok(None);
    };
    let request = GetOutlineRequest {
        document_id: Some(document_id.to_owned()),
        root_section_id: None,
        depth: 0,
        limit: OUTLINE_LIMIT,
        cursor: None,
    };
    let outline = engine.get_outline(request.clone())?;
    let meta = &document.meta;
    let mut text = format!(
        "# {}\n\n{} manual, revision {}, document ID `{}`. Read a section with the resource `x86://sections/{{section_id}}` or the x86_get_section tool.\n\n",
        meta.entry_path,
        vendor_label(meta.vendor),
        document.revision.as_deref().unwrap_or("unknown"),
        meta.document_id,
    );
    for node in &outline.nodes {
        let _ = writeln!(
            text,
            "- {} (`{}`)",
            node.section.heading, node.section.section_id
        );
    }
    if let Some(cursor) = outline.next_cursor {
        let next = GetOutlineRequest {
            cursor: Some(cursor),
            ..request
        };
        let _ = writeln!(
            text,
            "\nOnly the first {OUTLINE_LIMIT} top-level sections are listed. Continue with x86_get_outline {}.",
            serde_json::to_string(&next).unwrap_or_default()
        );
    }
    Ok(Some(text))
}

/// Renders a section (or a single block) with its citation, its blocks up to
/// [`SECTION_BUDGET_BYTES`] without running page headers and footers, and its
/// direct subsections.
fn section_text(engine: &QueryEngine, id: &str) -> Result<Option<String>, QueryError> {
    let mut request = GetSectionRequest {
        id: id.to_owned(),
        block_limit: SECTION_PAGE_BLOCKS,
        cursor: None,
        include_neighbors: false,
    };
    let first = engine.get_section(request.clone())?;
    let Some(citation) = first.citation.clone() else {
        return Ok(None);
    };
    let mut text = String::new();
    write_heading(&mut text, &citation, id);
    let children = first.children.clone();
    let mut complete = true;
    if let Some(block) = &first.block {
        complete = push_block(&mut text, &block.raw_source);
    } else {
        let mut page = first;
        'pages: loop {
            for block in page
                .section
                .iter()
                .flat_map(|section| &section.blocks)
                .filter(|block| block.content_class != ContentClass::PageFurniture)
            {
                if !push_block(&mut text, &block.raw_source) {
                    complete = false;
                    break 'pages;
                }
            }
            let Some(cursor) = page.next_cursor else {
                break;
            };
            request.cursor = Some(cursor);
            page = engine.get_section(request.clone())?;
        }
    }
    if !complete {
        let _ = writeln!(
            text,
            "\n_Truncated after {} KiB. Read all of it with x86_get_section {}._",
            SECTION_BUDGET_BYTES / 1024,
            json!({ "id": id, "block_limit": SECTION_PAGE_BLOCKS })
        );
    }
    if !children.is_empty() {
        text.push_str("\n## Subsections\n\n");
        for child in &children {
            let _ = writeln!(
                text,
                "- {} (`{}`)",
                child.section.heading, child.section.section_id
            );
        }
    }
    Ok(Some(text))
}

fn write_heading(text: &mut String, citation: &Citation, id: &str) {
    let span = &citation.span;
    let _ = write!(
        text,
        "# {}\n\n{} `{}`, lines {}-{}",
        citation.heading_path.join(" > "),
        vendor_label(citation.vendor),
        citation.entry_path,
        span.line_start,
        span.line_end,
    );
    if let Some(page) = &span.printed_page {
        let _ = write!(text, ", printed page {page}");
    }
    let _ = writeln!(text, ". ID `{id}`.");
}

/// Appends a block, cut at the remaining budget; `false` once the budget is exhausted.
fn push_block(text: &mut String, source: &str) -> bool {
    let source = source.trim_end();
    let remaining = SECTION_BUDGET_BYTES.saturating_sub(text.len() + 2);
    let mut end = source.len().min(remaining);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    text.push('\n');
    text.push_str(&source[..end]);
    text.push('\n');
    end == source.len()
}

/// Decodes `%XX` escapes; URI template clients encode the `:` in `sec:...` IDs.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|decoded| !decoded.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_budget_cuts_multibyte_blocks_on_char_boundaries() {
        // Five bytes remain after the block's two newlines, which ends inside the third 'é'.
        let mut text = "#".repeat(SECTION_BUDGET_BYTES - 7);
        assert!(!push_block(&mut text, &"é".repeat(8)));
        assert!(text.ends_with("\néé\n"));
        assert!(text.len() <= SECTION_BUDGET_BYTES);
    }
}
