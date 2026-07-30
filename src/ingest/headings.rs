use std::collections::HashMap;

use crate::domain::source::SourceSpan;

use super::SectionNode;

#[derive(Debug)]
struct ActiveHeading {
    level: u8,
    heading: String,
    section_id: String,
}

#[derive(Debug, Default)]
pub(crate) struct HeadingBuilder {
    active: Vec<ActiveHeading>,
    ordinals: HashMap<String, u32>,
}

impl HeadingBuilder {
    pub(crate) fn push(
        &mut self,
        document_id: &str,
        source_level: u8,
        heading: String,
        span: SourceSpan,
    ) -> SectionNode {
        let level = semantic_level(source_level, &heading);
        while self
            .active
            .last()
            .is_some_and(|active| active.level >= level)
        {
            self.active.pop();
        }

        let mut heading_path = self
            .active
            .iter()
            .map(|active| active.heading.clone())
            .collect::<Vec<_>>();
        heading_path.push(heading.clone());
        let normalized_path = heading_path
            .iter()
            .map(|part| part.to_lowercase())
            .collect::<Vec<_>>()
            .join("\0");
        let ordinal = self.ordinals.entry(normalized_path.clone()).or_default();
        *ordinal += 1;
        let ordinal = *ordinal;
        let identity = format!("{document_id}\0{normalized_path}\0{ordinal}");
        let section_id = format!("sec:{}", blake3::hash(identity.as_bytes()).to_hex());
        let parent_section_id = self.active.last().map(|active| active.section_id.clone());

        self.active.push(ActiveHeading {
            level,
            heading: heading.clone(),
            section_id: section_id.clone(),
        });
        SectionNode {
            section_id,
            parent_section_id,
            level,
            heading,
            heading_path,
            ordinal,
            span,
            printed_page: None,
        }
    }

    pub(crate) fn current(&self) -> Option<(&str, Vec<String>)> {
        self.active.last().map(|active| {
            (
                active.section_id.as_str(),
                self.active
                    .iter()
                    .map(|heading| heading.heading.clone())
                    .collect(),
            )
        })
    }
}

fn semantic_level(source_level: u8, heading: &str) -> u8 {
    let trimmed = heading.trim();
    if trimmed.to_ascii_uppercase().starts_with("CHAPTER ")
        || trimmed.to_ascii_uppercase().starts_with("APPENDIX ")
    {
        return 1;
    }
    let prefix = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    if prefix
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.')
        && prefix.chars().any(|character| character == '.')
    {
        let depth = prefix.matches('.').count().saturating_add(1);
        return u8::try_from(depth.clamp(1, 6)).unwrap_or(6);
    }
    source_level.clamp(1, 6)
}
