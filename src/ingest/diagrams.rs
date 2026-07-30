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
        if let Some((left, style, label, right)) = parse_edge(line) {
            let from = parse_endpoint(
                left,
                &mut nodes,
                &mut node_positions,
                &mut search_labels,
                &mut warnings,
                line_number + 1,
            );
            let to = parse_endpoint(
                right,
                &mut nodes,
                &mut node_positions,
                &mut search_labels,
                &mut warnings,
                line_number + 1,
            );
            if let Some(label) = &label {
                push_unique(&mut search_labels, label);
            }
            if !from.is_empty() && !to.is_empty() {
                edges.push(DiagramEdge {
                    from,
                    to,
                    label,
                    style,
                });
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

fn parse_edge(line: &str) -> Option<(&str, String, Option<String>, &str)> {
    static PIPE_LABELED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?P<left>.+?)\s*(?P<arrow><==>|<-->|<---|--->|-\.->|-->|==>|---|--[xX])\s*\|(?P<label>[^|]*)\|\s*(?P<right>.+)$",
        )
        .expect("Mermaid pipe-labeled edge regex must compile")
    });
    static LABELED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"^(?P<left>.+?)\s*(?P<open>--|-\.)\s*(?:"(?P<quoted>[^"]+)"|(?P<plain>[^\-.][^>]*?))?\s*(?P<close>-->|\.->)\s*(?P<right>.+)$"#,
        )
        .expect("Mermaid labeled-edge regex must compile")
    });
    static SIMPLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?P<left>.+?)\s*(?P<arrow><==>|<-->|<---|--->|-\.->|-->|==>|---|--[xX])\s*(?P<right>.+)$",
        )
        .expect("Mermaid edge regex must compile")
    });
    if let Some(captures) = PIPE_LABELED.captures(line) {
        let left = captures.name("left")?.as_str();
        let right = captures.name("right")?.as_str();
        let arrow = captures.name("arrow")?.as_str();
        let label = captures
            .name("label")
            .map(|value| value.as_str().trim().to_owned())
            .filter(|value| !value.is_empty());
        return Some(orient_edge(left, arrow, label, right));
    }
    if let Some(captures) = LABELED.captures(line) {
        let left = captures.name("left")?.as_str();
        let right = captures.name("right")?.as_str();
        let open = captures.name("open")?.as_str();
        let close = captures.name("close")?.as_str();
        let label = captures
            .name("quoted")
            .or_else(|| captures.name("plain"))
            .map(|value| value.as_str().trim().to_owned())
            .filter(|value| !value.is_empty());
        return Some((left, format!("{open}{close}"), label, right));
    }
    SIMPLE.captures(line).and_then(|captures| {
        let left = captures.name("left")?.as_str();
        let right = captures.name("right")?.as_str();
        let arrow = captures.name("arrow")?.as_str();
        Some(orient_edge(left, arrow, None, right))
    })
}

fn orient_edge<'a>(
    left: &'a str,
    style: &str,
    label: Option<String>,
    right: &'a str,
) -> (&'a str, String, Option<String>, &'a str) {
    if style.starts_with('<') && !style.ends_with('>') {
        (right, style.to_owned(), label, left)
    } else {
        (left, style.to_owned(), label, right)
    }
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
    let (label, malformed) = parse_node_label(remainder, &id);
    if malformed {
        warnings.push(ExtractionWarning {
            code: "malformed_mermaid_node".into(),
            message: format!("line {line_number} has an unclosed node label for {id}"),
        });
    }
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

fn parse_node_label(remainder: &str, fallback: &str) -> (String, bool) {
    if remainder.is_empty() {
        return (fallback.to_owned(), false);
    }
    let opening = remainder.chars().next().unwrap_or_default();
    if !matches!(opening, '[' | '(' | '{') {
        return (fallback.to_owned(), false);
    }
    let closing = match opening {
        '[' => ']',
        '(' => ')',
        '{' => '}',
        _ => unreachable!(),
    };
    let closed = remainder.ends_with(closing);
    let value = remainder
        .trim_matches(|character| matches!(character, '[' | ']' | '(' | ')' | '{' | '}' | '"'))
        .trim();
    let label = value.split("-->").next().unwrap_or(value).trim();
    (
        if label.is_empty() {
            fallback.to_owned()
        } else {
            label.to_owned()
        },
        !closed,
    )
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
}
