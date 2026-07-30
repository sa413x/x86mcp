use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser};
use regex::Regex;

use crate::domain::block::ContentClass;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeadingDisposition {
    Section,
    Metadata(ContentClass),
}

pub(crate) fn normalize_markdown(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    for event in Parser::new_ext(raw, Options::all()) {
        match event {
            Event::Text(value)
            | Event::Code(value)
            | Event::InlineMath(value)
            | Event::DisplayMath(value)
            | Event::FootnoteReference(value) => push_piece(&mut text, &value),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::TaskListMarker(checked) => {
                push_piece(&mut text, if checked { "checked" } else { "unchecked" });
            }
            _ => {}
        }
    }
    collapse_whitespace(&text)
}

pub(crate) fn classify_heading(heading: &str, before_substantive: bool) -> HeadingDisposition {
    let lower = heading.to_lowercase();
    if lower.contains("notices")
        || lower.contains("disclaimer")
        || lower.contains("legal information")
    {
        return HeadingDisposition::Metadata(ContentClass::Legal);
    }
    if lower == "contents"
        || lower.ends_with(" contents")
        || lower.contains("table of contents")
        || lower == "figures"
        || lower == "tables"
    {
        return HeadingDisposition::Metadata(ContentClass::Contents);
    }
    if lower.contains("revision history") || lower.contains("revision guide") {
        return HeadingDisposition::Metadata(ContentClass::RevisionHistory);
    }
    if before_substantive
        && (lower.contains("architectures software developer")
            || lower.contains("amd64 architecture programmer")
            || lower.starts_with("volume ")
            || lower.contains("publication no."))
    {
        return HeadingDisposition::Metadata(ContentClass::FrontMatter);
    }
    HeadingDisposition::Section
}

pub(crate) fn classify_body(
    normalized: &str,
    inherited: ContentClass,
    known_headings: &[String],
) -> ContentClass {
    if is_page_furniture(normalized, known_headings) {
        ContentClass::PageFurniture
    } else if inherited == ContentClass::FrontMatter && looks_legal(normalized) {
        ContentClass::Legal
    } else {
        inherited
    }
}

pub(crate) fn extract_printed_page(raw: &str) -> Option<String> {
    static PAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:[A-Z]{1,4}-)?\d{1,4}-\d{1,4}\b")
            .expect("printed-page regex must compile")
    });
    PAGE.find(raw).map(|value| value.as_str().to_owned())
}

pub(crate) fn is_page_marker(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("<page_number>")
        || (extract_printed_page(raw).is_some()
            && (lower.contains("vol.") || lower.contains("volume")))
}

fn is_page_furniture(normalized: &str, known_headings: &[String]) -> bool {
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "intel logo"
        || lower == "amd logo"
        || (trimmed.len() < 48 && (lower.starts_with("vol. ") || lower.starts_with("volume ")))
    {
        return true;
    }
    if trimmed.len() <= 120
        && trimmed.chars().any(char::is_alphabetic)
        && trimmed
            .chars()
            .filter(|character| character.is_alphabetic())
            .all(|character| character.is_uppercase())
    {
        let canonical = canonical_heading(trimmed);
        return known_headings.iter().any(|heading| {
            let known = canonical_heading(heading);
            known == canonical || known.ends_with(&canonical) || canonical.ends_with(&known)
        });
    }
    false
}

fn looks_legal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("all rights reserved")
        || lower.contains("no product or component")
        || lower.contains("warranties")
        || lower.contains("copyright")
}

fn canonical_heading(value: &str) -> String {
    let without_number = value
        .trim()
        .trim_start_matches(|character: char| character.is_ascii_digit() || character == '.')
        .trim_start();
    without_number
        .strip_prefix("CHAPTER ")
        .unwrap_or(without_number)
        .to_owned()
}

fn push_piece(target: &mut String, value: &str) {
    if !target.is_empty()
        && !target.ends_with(char::is_whitespace)
        && !value.starts_with(char::is_whitespace)
    {
        target.push(' ');
    }
    target.push_str(value);
}

fn collapse_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut whitespace = false;
    for character in value.chars() {
        if character.is_whitespace() {
            whitespace = !output.is_empty();
        } else {
            if whitespace {
                output.push(' ');
                whitespace = false;
            }
            output.push(character);
        }
    }
    output
}
