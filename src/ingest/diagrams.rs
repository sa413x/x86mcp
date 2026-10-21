use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::block::{BlockKind, SourceBlock};

use super::ExtractionWarning;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub style: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ExtractedDiagram {
    pub diagram_id: String,
    pub source_block_id: String,
    pub direction: Option<String>,
    pub caption: Option<String>,
    pub nodes: Vec<DiagramNode>,
    pub edges: Vec<DiagramEdge>,
    pub subgraphs: Vec<String>,
    pub search_labels: Vec<String>,
    pub raw_source: String,
    pub warnings: Vec<ExtractionWarning>,
}

#[derive(Debug, Error)]
pub enum DiagramError {
    #[error("source block {block_id} is not a Mermaid diagram")]
    WrongBlockKind { block_id: String },
}

pub fn extract_diagram(
    block: &SourceBlock,
    caption: Option<&str>,
) -> Result<ExtractedDiagram, DiagramError> {
    if block.kind != BlockKind::Diagram {
        return Err(DiagramError::WrongBlockKind {
            block_id: block.block_id.clone(),
        });
    }
    let mut nodes = Vec::new();
    let mut node_positions = HashMap::new();
    let mut edges = Vec::new();
    let mut subgraphs = Vec::new();
    let mut search_labels = Vec::new();
    let mut warnings = Vec::new();
    let mut direction = None;
    let lines = mermaid_body(&block.raw_source);

    for (line_number, line) in lines.lines().enumerate() {
        let line = line.trim().trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if line == "stateDiagram-v2" || line.starts_with("style ") {
            continue;
        }
        if let Some(value) = line
            .strip_prefix("graph ")
            .or_else(|| line.strip_prefix("flowchart "))
        {
            direction = value.split_whitespace().next().map(str::to_owned);
            continue;
        }
        if let Some(value) = line.strip_prefix("subgraph ") {
            let label = parse_subgraph_label(value);
            push_unique(&mut search_labels, &label);
            subgraphs.push(label);
            continue;
        }
        if line == "end" || line.starts_with("direction ") {
            continue;
        }
        if let Some(statement) = parse_edge_statement(line) {
            let groups = statement
                .groups
                .iter()
                .map(|group| {
                    group
                        .iter()
                        .map(|endpoint| {
                            parse_endpoint(
                                endpoint,
                                &mut nodes,
                                &mut node_positions,
                                &mut search_labels,
                                &mut warnings,
                                line_number + 1,
                            )
                        })
                        .filter(|id| !id.is_empty())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            for (position, link) in statement.links.into_iter().enumerate() {
                if let Some(label) = &link.label {
                    push_unique(&mut search_labels, label);
                }
                let reversed = link.style.starts_with('<') && !link.style.ends_with('>');
                for left in &groups[position] {
                    for right in &groups[position + 1] {
                        let (from, to) = if reversed {
                            (right, left)
                        } else {
                            (left, right)
                        };
                        edges.push(DiagramEdge {
                            from: from.clone(),
                            to: to.clone(),
                            label: link.label.clone(),
                            style: link.style.clone(),
                        });
                    }
                }
            }
            continue;
        }
        if looks_like_node(line) {
            parse_endpoint(
                line,
                &mut nodes,
                &mut node_positions,
                &mut search_labels,
                &mut warnings,
                line_number + 1,
            );
            continue;
        }

        collect_plain_labels(line, &mut search_labels);
        warnings.push(ExtractionWarning {
            code: "unknown_mermaid_syntax".into(),
            message: format!(
                "line {} was retained but not structurally parsed",
                line_number + 1
            ),
        });
    }
    if let Some(caption) = caption {
        push_unique(&mut search_labels, caption);
    }
    let identity = format!("{}\0{}", block.document_id, block.block_id);
    Ok(ExtractedDiagram {
        diagram_id: format!("fig:{}", blake3::hash(identity.as_bytes()).to_hex()),
        source_block_id: block.block_id.clone(),
        direction,
        caption: caption.map(str::to_owned),
        nodes,
        edges,
        subgraphs,
        search_labels,
        raw_source: block.raw_source.clone(),
        warnings,
    })
}

fn mermaid_body(raw: &str) -> &str {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") && !trimmed.starts_with("~~~") {
        return trimmed;
    }
    let after_header = trimmed.split_once('\n').map_or("", |(_, body)| body);
    after_header
        .strip_suffix("```")
        .or_else(|| after_header.strip_suffix("~~~"))
        .unwrap_or(after_header)
        .trim_end()
}

/// A link between two endpoint groups, e.g. `-->`, `-- label -->`, `-.->|label|`.
#[derive(Debug)]
struct Link {
    style: String,
    label: Option<String>,
}

/// An edge statement: endpoint groups (`A & B`) joined by links, possibly
/// chained (`A --- B --- C`).
#[derive(Debug)]
struct EdgeStatement<'a> {
    groups: Vec<Vec<&'a str>>,
    links: Vec<Link>,
}

fn parse_edge_statement(line: &str) -> Option<EdgeStatement<'_>> {
    // Unbalanced node shapes would hide every link; fall back to splitting on
    // links anywhere so the malformed endpoint is reported and its labels kept.
    let (segments, mut links) = split_links(line, true).or_else(|| split_links(line, false))?;
    let last = segments.len() - 1;
    let groups = segments
        .iter()
        .enumerate()
        .map(|(position, &segment)| {
            let mut endpoints = segment;
            if position == last {
                // State-diagram transitions carry their label after `:`.
                if let [endpoint, label] = split_top_level(segment, ':')[..] {
                    if let Some(label) = text_label(label) {
                        links[last - 1].label.get_or_insert(label);
                    }
                    endpoints = endpoint;
                }
            }
            split_top_level(endpoints, '&')
        })
        .collect();
    Some(EdgeStatement { groups, links })
}

/// Splits a statement at every link outside node shapes and quotes. Returns
/// `None` when there is no link or, with `track_shapes`, when brackets are
/// unbalanced.
fn split_links(line: &str, track_shapes: bool) -> Option<(Vec<&str>, Vec<Link>)> {
    let mut segments = Vec::new();
    let mut links = Vec::new();
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut segment_start = 0_usize;
    let mut index = 0_usize;
    while let Some(character) = line[index..].chars().next() {
        if quoted {
            quoted = character != '"';
        } else if character == '"' && track_shapes {
            quoted = true;
        } else if matches!(character, '[' | '(' | '{') && track_shapes {
            depth += 1;
        } else if matches!(character, ']' | ')' | '}') && track_shapes {
            depth = depth.checked_sub(1)?;
        } else if depth == 0
            && let Some((length, link)) = match_link(&line[index..])
        {
            segments.push(&line[segment_start..index]);
            links.push(link);
            index += length;
            segment_start = index;
            continue;
        }
        index += character.len_utf8();
    }
    if links.is_empty() || depth != 0 || quoted {
        return None;
    }
    segments.push(&line[segment_start..]);
    Some((segments, links))
}

/// Splits on `separator` outside node shapes and quotes.
fn split_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut start = 0_usize;
    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth = depth.saturating_sub(1),
            _ if character == separator && !quoted && depth == 0 => {
                parts.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// Matches one Mermaid link at the start of `source`: `-->`, `---`, `==>`,
/// `-.->`, `<-->`, `--x`, `~~~`, text-labelled forms (`-- text -->`,
/// `== text ==>`, `-. text .->`) and a trailing `|label|`. Returns the
/// consumed length.
fn match_link(source: &str) -> Option<(usize, Link)> {
    let bytes = source.as_bytes();
    let start = usize::from(bytes.first() == Some(&b'<'));
    let (mut end, style, mut label) = if source[start..].starts_with("~~~") {
        let end = start + run_length(&bytes[start..], b'~');
        (end, source[..end].to_owned(), None)
    } else if source[start..].starts_with("-.") {
        let dots = run_length(&bytes[start + 1..], b'.');
        let after = start + 1 + dots;
        if bytes.get(after) == Some(&b'-') {
            let end = after + 1 + usize::from(bytes.get(after + 1) == Some(&b'>'));
            (end, source[..end].to_owned(), None)
        } else {
            let (close_start, close_end) = find_close(source, after, b'.')?;
            (
                close_end,
                format!("{}-{}", &source[..start], &source[close_start..close_end]),
                text_label(&source[after..close_start]),
            )
        }
    } else if source[start..].starts_with("--") || source[start..].starts_with("==") {
        let stroke = bytes[start];
        let after = start + run_length(&bytes[start..], stroke);
        let terminates = |position: usize| {
            bytes
                .get(position)
                .is_none_or(|byte| byte.is_ascii_whitespace())
        };
        match bytes.get(after) {
            Some(b'>') => (after + 1, source[..after + 1].to_owned(), None),
            Some(b'x' | b'X' | b'o') if terminates(after + 1) => {
                (after + 1, source[..after + 1].to_owned(), None)
            }
            _ if after - start >= 3 => (after, source[..after].to_owned(), None),
            _ => {
                let (close_start, close_end) = find_close(source, after, stroke)?;
                (
                    close_end,
                    format!("{}{}", &source[..start], &source[close_start..close_end]),
                    text_label(&source[after..close_start]),
                )
            }
        }
    } else {
        return None;
    };
    let after_arrow = end + run_length_by(&bytes[end..], |byte| byte == b' ');
    if bytes.get(after_arrow) == Some(&b'|')
        && let Some(close) = source[after_arrow + 1..].find('|')
    {
        label = text_label(&source[after_arrow + 1..after_arrow + 1 + close]);
        end = after_arrow + close + 2;
    }
    Some((end, Link { style, label }))
}

/// Finds the closing half of a text-labelled link after `from`: `-->`/`---`
/// for `--`, `==>`/`===` for `==`, `.->`/`.-` for `-.`.
fn find_close(source: &str, from: usize, stroke: u8) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut position = from;
    while position < bytes.len() {
        if bytes[position] == b'"' {
            position += 1 + source[position + 1..].find('"')? + 1;
            continue;
        }
        if stroke == b'.' {
            if bytes[position] == b'.' && bytes.get(position + 1) == Some(&b'-') {
                let mut end = position + 1 + run_length(&bytes[position + 1..], b'-');
                end += usize::from(bytes.get(end) == Some(&b'>'));
                return Some((position, end));
            }
        } else if bytes[position] == stroke {
            let run = run_length(&bytes[position..], stroke);
            if bytes.get(position + run) == Some(&b'>') && run >= 2 {
                return Some((position, position + run + 1));
            }
            if run >= 3 {
                return Some((position, position + run));
            }
            position += run;
            continue;
        }
        position += 1;
    }
    None
}

fn text_label(value: &str) -> Option<String> {
    let label = value.trim().trim_matches('"').trim();
    (!label.is_empty()).then(|| label.to_owned())
}

fn run_length(bytes: &[u8], expected: u8) -> usize {
    run_length_by(bytes, |byte| byte == expected)
}

fn run_length_by(bytes: &[u8], predicate: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|byte| predicate(**byte)).count()
}

#[allow(clippy::too_many_arguments)]
fn parse_endpoint(
    source: &str,
    nodes: &mut Vec<DiagramNode>,
    positions: &mut HashMap<String, usize>,
    search_labels: &mut Vec<String>,
    warnings: &mut Vec<ExtractionWarning>,
    line_number: usize,
) -> String {
    let source = source.trim();
    let (id, remainder) = if let Some(remainder) = source.strip_prefix("[*]") {
        ("[*]".to_owned(), remainder.trim())
    } else {
        let id_length = source
            .char_indices()
            .take_while(|(_, character)| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
            .last()
            .map_or(0, |(index, character)| index + character.len_utf8());
        (source[..id_length].to_owned(), source[id_length..].trim())
    };
    if id.is_empty() {
        warnings.push(ExtractionWarning {
            code: "malformed_mermaid_node".into(),
            message: format!("line {line_number} has an endpoint without an identifier"),
        });
        collect_plain_labels(source, search_labels);
        return id;
    }
    let label = match parse_node_label(remainder, &id) {
        Some((label, malformed)) => {
            if malformed {
                warnings.push(ExtractionWarning {
                    code: "malformed_mermaid_node".into(),
                    message: format!("line {line_number} has an unclosed node label for {id}"),
                });
            }
            label
        }
        None => {
            warnings.push(ExtractionWarning {
                code: "unknown_mermaid_syntax".into(),
                message: format!(
                    "line {line_number} has text after node {id} that was retained but not structurally parsed"
                ),
            });
            collect_plain_labels(remainder, search_labels);
            id.clone()
        }
    };
    push_unique(search_labels, &label);
    if let Some(position) = positions.get(&id).copied() {
        if nodes[position].label == nodes[position].id && label != id {
            nodes[position].label = label;
        }
    } else {
        positions.insert(id.clone(), nodes.len());
        nodes.push(DiagramNode {
            id: id.clone(),
            label,
        });
    }
    id
}

/// Parses the shape after a node id. `None` means the remainder is not a
/// node shape; the flag reports an unclosed shape.
fn parse_node_label(remainder: &str, fallback: &str) -> Option<(String, bool)> {
    if remainder.is_empty() {
        return Some((fallback.to_owned(), false));
    }
    let closing = match remainder.chars().next()? {
        '[' | '>' => ']',
        '(' => ')',
        '{' => '}',
        _ => return None,
    };
    let closed = remainder.ends_with(closing);
    let value = remainder
        .trim_matches(|character| {
            matches!(
                character,
                '[' | ']' | '(' | ')' | '{' | '}' | '>' | '"' | '/' | '\\'
            )
        })
        .trim();
    let label = value.split("-->").next().unwrap_or(value).trim();
    Some((
        if label.is_empty() {
            fallback.to_owned()
        } else {
            label.to_owned()
        },
        !closed,
    ))
}

fn parse_subgraph_label(value: &str) -> String {
    let value = value.trim();
    if let Some(open) = value.find('[') {
        return value[open + 1..]
            .trim_end_matches(']')
            .trim_matches('"')
            .to_owned();
    }
    value.trim_matches('"').to_owned()
}

fn looks_like_node(line: &str) -> bool {
    let starts_with_identifier = line
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
    starts_with_identifier
        && (line
            .chars()
            .any(|character| matches!(character, '[' | '(' | '{'))
            || line.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            }))
}

fn collect_plain_labels(line: &str, labels: &mut Vec<String>) {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"[\[({]\s*"?([^\]})"-]{2,})"?[\]})]?"#)
            .expect("Mermaid fallback-label regex must compile")
    });
    for captures in LABEL.captures_iter(line) {
        if let Some(value) = captures.get(1) {
            push_unique(labels, value.as_str().trim());
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        block::{BlockKind, ContentClass, SourceBlock},
        source::SourceSpan,
    };

    use super::extract_diagram;

    fn diagram_block(raw_source: &str) -> SourceBlock {
        SourceBlock {
            block_id: "blk:diagram".into(),
            document_id: "doc:test".into(),
            section_id: "sec:test".into(),
            kind: BlockKind::Diagram,
            heading_path: vec!["CHAPTER 26".into()],
            raw_source: raw_source.into(),
            normalized_text: raw_source.into(),
            content_class: ContentClass::Substantive,
            span: SourceSpan::default(),
        }
    }

    #[test]
    fn extracts_subgraphs_labels_and_labeled_edges() {
        let block = diagram_block(
            "```mermaid\ngraph TD\nsubgraph Guests\nG1[Guest 1]\nend\nVMM[VM Monitor] -- \"VM Entry\" --> G1\nG1 -. \"VM Exit\" .-> VMM\n```\n",
        );
        let caption = "Figure 26-1. Interaction of a Virtual-Machine Monitor and Guests";
        let diagram = extract_diagram(&block, Some(caption)).unwrap();
        assert_eq!(diagram.caption.as_deref(), Some(caption));
        assert!(
            diagram
                .nodes
                .iter()
                .any(|node| node.id == "G1" && node.label == "Guest 1")
        );
        assert!(diagram.edges.iter().any(|edge| edge.from == "VMM"
            && edge.to == "G1"
            && edge.label.as_deref() == Some("VM Entry")));
        assert!(
            diagram
                .subgraphs
                .iter()
                .any(|subgraph| subgraph == "Guests")
        );
    }

    #[test]
    fn extracts_pipe_labeled_edges() {
        let block = diagram_block(
            "```mermaid\ngraph TD\nA[Start] -->|Yes| B[Done]\nB -.->|PMI| C[Handler]\n```\n",
        );
        let diagram = extract_diagram(&block, None).unwrap();

        assert!(diagram.warnings.is_empty());
        assert!(diagram.edges.iter().any(|edge| {
            edge.from == "A" && edge.to == "B" && edge.label.as_deref() == Some("Yes")
        }));
        assert!(diagram.edges.iter().any(|edge| {
            edge.from == "B" && edge.to == "C" && edge.label.as_deref() == Some("PMI")
        }));
    }

    #[test]
    fn extracts_extended_flowchart_arrows() {
        let block = diagram_block(
            "```mermaid\ngraph TD\nA <--> B\nC <==>|QPI| D\nE <--- F\nG ---> H\nI --X J\n```\n",
        );
        let diagram = extract_diagram(&block, None).unwrap();

        assert!(diagram.warnings.is_empty());
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| { edge.from == "A" && edge.to == "B" && edge.style == "<-->" })
        );
        assert!(diagram.edges.iter().any(|edge| {
            edge.from == "C"
                && edge.to == "D"
                && edge.style == "<==>"
                && edge.label.as_deref() == Some("QPI")
        }));
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| { edge.from == "F" && edge.to == "E" && edge.style == "<---" })
        );
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| { edge.from == "G" && edge.to == "H" && edge.style == "--->" })
        );
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| { edge.from == "I" && edge.to == "J" && edge.style == "--X" })
        );
    }

    #[test]
    fn accepts_standalone_nodes_and_style_directives() {
        let block =
            diagram_block("```mermaid\ngraph TD\nGDT\nstyle GDT fill:none,stroke:#000\n```\n");
        let diagram = extract_diagram(&block, None).unwrap();

        assert!(diagram.warnings.is_empty());
        assert!(
            diagram
                .nodes
                .iter()
                .any(|node| node.id == "GDT" && node.label == "GDT")
        );
    }

    #[test]
    fn extracts_state_diagram_start_node() {
        let block = diagram_block("```mermaid\nstateDiagram-v2\n[*] --> Invalid: Reset\n```\n");
        let diagram = extract_diagram(&block, None).unwrap();

        assert!(diagram.warnings.is_empty());
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| edge.from == "[*]" && edge.to == "Invalid")
        );
    }

    #[test]
    fn malformed_mermaid_retains_raw_and_best_effort_labels() {
        let block = diagram_block("```mermaid\ngraph TD\nA[Alpha --> B[Beta]\n```\n");
        let diagram = extract_diagram(&block, None).unwrap();
        assert_eq!(diagram.raw_source, block.raw_source);
        assert!(diagram.search_labels.iter().any(|label| label == "Alpha"));
        assert!(!diagram.warnings.is_empty());
    }

    fn edge_triples(source: &str) -> Vec<(String, String, Option<String>)> {
        extract_diagram(&diagram_block(source), None)
            .unwrap()
            .edges
            .into_iter()
            .map(|edge| (edge.from, edge.to, edge.label))
            .collect()
    }

    fn triple(from: &str, to: &str, label: Option<&str>) -> (String, String, Option<String>) {
        (from.into(), to.into(), label.map(str::to_owned))
    }

    #[test]
    fn chained_edges_produce_every_link() {
        let edges = edge_triples(
            "```mermaid\ngraph LR\nA_OLD_TOP --- A_DATA --- A_NEW_TOP --- A_BASE\nB1[31 ... 24] --- B2[23 ... 16] -- low --> B3\n```\n",
        );
        assert_eq!(
            edges,
            vec![
                triple("A_OLD_TOP", "A_DATA", None),
                triple("A_DATA", "A_NEW_TOP", None),
                triple("A_NEW_TOP", "A_BASE", None),
                triple("B1", "B2", None),
                triple("B2", "B3", Some("low")),
            ]
        );
    }

    #[test]
    fn ampersand_groups_fan_out_across_links() {
        let edges = edge_triples(
            "```mermaid\ngraph TD\nPM_S & PM_EA --> PM_Seg\nLP0 & LP1 --- L12[\"L1 & L2\"] -->|QPI| X & Y\n```\n",
        );
        assert_eq!(
            edges,
            vec![
                triple("PM_S", "PM_Seg", None),
                triple("PM_EA", "PM_Seg", None),
                triple("LP0", "L12", None),
                triple("LP1", "L12", None),
                triple("L12", "X", Some("QPI")),
                triple("L12", "Y", Some("QPI")),
            ]
        );
    }

    #[test]
    fn unparsed_endpoint_text_is_reported() {
        let diagram = extract_diagram(
            &diagram_block("```mermaid\ngraph TD\nA --> B extra\n```\n"),
            None,
        )
        .unwrap();
        assert!(
            diagram
                .warnings
                .iter()
                .any(|warning| warning.code == "unknown_mermaid_syntax")
        );
        assert!(
            diagram
                .edges
                .iter()
                .any(|edge| edge.from == "A" && edge.to == "B")
        );
    }
}
