use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;

use crate::domain::{
    block::{BlockKind, SourceBlock},
    reference::{ReferenceKind, ReferenceRecord},
};

use super::{ExtractedDiagram, ExtractedTable, ParsedDocument, SectionNode};
pub fn extract_references(block: &SourceBlock) -> Vec<ReferenceRecord> {
    if block.kind == BlockKind::Caption
        || block
            .heading_path
            .last()
            .is_some_and(|heading| heading == &block.normalized_text)
    {
        return Vec::new();
    }
    static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?P<kind>Section|Chapter|Appendix|Table|Figure)\s+(?P<label>[A-Z]|\d+(?:\.\d+)*(?:-\d+)?)",
        )
        .expect("manual-reference regex must compile")
    });

    REFERENCE
        .captures_iter(&block.raw_source)
        .filter_map(|captures| {
            let matched = captures.get(0)?;
            let kind_name = captures.name("kind")?.as_str().to_ascii_lowercase();
            let value = captures.name("label")?.as_str();
            let (kind, normalized_key) = match kind_name.as_str() {
                "table" => (
                    ReferenceKind::Table,
                    format!("table:{}", value.to_ascii_lowercase()),
                ),
                "figure" => (
                    ReferenceKind::Figure,
                    format!("figure:{}", value.to_ascii_lowercase()),
                ),
                "chapter" => (
                    ReferenceKind::Section,
                    format!("chapter:{}", value.to_ascii_lowercase()),
                ),
                "appendix" => (
                    ReferenceKind::Section,
                    format!("appendix:{}", value.to_ascii_lowercase()),
                ),
                _ => (
                    ReferenceKind::Section,
                    format!("section:{}", value.to_ascii_lowercase()),
                ),
            };
            let identity = format!(
                "{}\0{}\0{}\0{}",
                block.document_id,
                block.block_id,
                matched.start(),
                normalized_key
            );
            Some(ReferenceRecord {
                reference_id: format!("ref:{}", blake3::hash(identity.as_bytes()).to_hex()),
                source_block_id: block.block_id.clone(),
                kind,
                label: matched.as_str().to_owned(),
                normalized_key,
                target_document_id: None,
                target_id: None,
                candidates: Vec::new(),
                resolved: false,
            })
        })
        .collect()
}

pub(crate) fn resolve_document_references(
    document_id: &str,
    sections: &[SectionNode],
    tables: &[ExtractedTable],
    diagrams: &[ExtractedDiagram],
    references: &mut [ReferenceRecord],
) {
    let declarations = declarations(sections, tables, diagrams);

    for reference in references {
        let mut candidates = declarations
            .get(&reference.normalized_key)
            .cloned()
            .unwrap_or_default();
        candidates.sort_unstable();
        candidates.dedup();
        reference.resolved = candidates.len() == 1;
        reference.target_document_id = reference.resolved.then(|| document_id.to_owned());
        reference.target_id = if reference.resolved {
            candidates.first().cloned()
        } else {
            None
        };
        reference.candidates = candidates;
    }
}

pub fn resolve_corpus_references(documents: &mut [ParsedDocument]) {
    let mut global = HashMap::<String, Vec<(String, String)>>::new();
    for document in documents.iter() {
        for (key, target_ids) in
            declarations(&document.sections, &document.tables, &document.diagrams)
        {
            global.entry(key).or_default().extend(
                target_ids
                    .into_iter()
                    .map(|target_id| (document.document_id.clone(), target_id)),
            );
        }
    }
    for candidates in global.values_mut() {
        candidates.sort_unstable();
        candidates.dedup();
    }

    for document in documents {
        for reference in &mut document.references {
            let global_candidates = global
                .get(&reference.normalized_key)
                .cloned()
                .unwrap_or_default();
            let local_candidates = global_candidates
                .iter()
                .filter(|(document_id, _)| document_id == &document.document_id)
                .cloned()
                .collect::<Vec<_>>();
            let candidates = if local_candidates.is_empty() {
                global_candidates
            } else {
                local_candidates
            };
            reference.resolved = candidates.len() == 1;
            reference.target_document_id = reference.resolved.then(|| candidates[0].0.clone());
            reference.target_id = reference.resolved.then(|| candidates[0].1.clone());
            reference.candidates = candidates
                .into_iter()
                .map(|(_, target_id)| target_id)
                .collect();
        }
    }
}

fn declarations(
    sections: &[SectionNode],
    tables: &[ExtractedTable],
    diagrams: &[ExtractedDiagram],
) -> HashMap<String, Vec<String>> {
    let mut declarations = HashMap::<String, Vec<String>>::new();
    for section in sections {
        if let Some(key) = section_key(&section.heading) {
            declarations
                .entry(key)
                .or_default()
                .push(section.section_id.clone());
        }
    }
    for table in tables {
        if let Some(key) = table
            .caption
            .as_deref()
            .and_then(|caption| caption_key(caption, "table"))
        {
            declarations
                .entry(key)
                .or_default()
                .push(table.table_id.clone());
        }
    }
    for diagram in diagrams {
        if let Some(key) = diagram
            .caption
            .as_deref()
            .and_then(|caption| caption_key(caption, "figure"))
        {
            declarations
                .entry(key)
                .or_default()
                .push(diagram.diagram_id.clone());
        }
    }
    declarations
}

fn section_key(heading: &str) -> Option<String> {
    static HEADING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(?:(?P<kind>CHAPTER|APPENDIX)\s+)?(?P<label>[A-Z]|\d+(?:\.\d+)*)\b")
            .expect("section declaration regex must compile")
    });
    let captures = HEADING.captures(heading)?;
    let label = captures.name("label")?.as_str().to_ascii_lowercase();
    Some(
        match captures
            .name("kind")
            .map(|kind| kind.as_str().to_ascii_lowercase())
            .as_deref()
        {
            Some("chapter") => format!("chapter:{label}"),
            Some("appendix") => format!("appendix:{label}"),
            _ => format!("section:{label}"),
        },
    )
}

fn caption_key(caption: &str, expected_kind: &str) -> Option<String> {
    static CAPTION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(?P<kind>Table|Figure)\s+(?P<label>\d+(?:\.\d+)*(?:-\d+)?)\b")
            .expect("caption declaration regex must compile")
    });
    let captures = CAPTION.captures(caption)?;
    let kind = captures.name("kind")?.as_str().to_ascii_lowercase();
    (kind == expected_kind).then(|| {
        format!(
            "{kind}:{}",
            captures
                .name("label")
                .unwrap()
                .as_str()
                .to_ascii_lowercase()
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        block::{BlockKind, ContentClass, SourceBlock},
        reference::ReferenceKind,
        source::SourceSpan,
    };

    use super::extract_references;

    #[test]
    fn extracts_typed_unresolved_manual_references() {
        let block = SourceBlock {
            block_id: "blk:refs".into(),
            document_id: "doc:test".into(),
            section_id: "sec:test".into(),
            kind: BlockKind::Prose,
            heading_path: vec!["CHAPTER 26".into()],
            raw_source: "See Section 26.8, Table 2-48, and Figure 26-1. Appendix A also applies."
                .into(),
            normalized_text:
                "See Section 26.8, Table 2-48, and Figure 26-1. Appendix A also applies.".into(),
            content_class: ContentClass::Substantive,
            span: SourceSpan::default(),
        };
        let references = extract_references(&block);
        assert_eq!(references.len(), 4);
        assert_eq!(references[0].kind, ReferenceKind::Section);
        assert_eq!(references[0].normalized_key, "section:26.8");
        assert_eq!(references[1].kind, ReferenceKind::Table);
        assert_eq!(references[2].kind, ReferenceKind::Figure);
        assert_eq!(references[3].kind, ReferenceKind::Section);
        assert!(!references[0].resolved);
        assert!(references[0].candidates.is_empty());
    }
}
