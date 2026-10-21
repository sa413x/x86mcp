use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser};
use regex::Regex;

use crate::domain::block::ContentClass;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeadingDisposition {
    Section,
    Metadata(ContentClass),
    /// A running page header converted into a heading. It belongs to the
    /// current section and must not change the inherited content class.
    PageFurniture,
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

pub(crate) fn classify_heading(
    heading: &str,
    before_substantive: bool,
    chapter_title: Option<&str>,
) -> HeadingDisposition {
    let normalized = normalize_markdown(heading);
    if is_running_header(&normalized)
        || chapter_title.is_some_and(|title| repeats_chapter_title(&normalized, title))
    {
        return HeadingDisposition::PageFurniture;
    }
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

/// Extracts the printed page label from a page-number element or a footer line.
pub(crate) fn extract_printed_page(raw: &str) -> Option<String> {
    static ELEMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<page_number>(?P<inner>.*?)(?:</page_number>|$)")
            .expect("page-number element regex must compile")
    });
    let Some(captures) = ELEMENT.captures(raw) else {
        return footer_label(raw);
    };
    let element = captures.get(0)?;
    let inner = captures.name("inner")?.as_str();
    page_label(inner).or_else(|| {
        let outside = format!("{} {}", &raw[..element.start()], &raw[element.end()..]);
        page_label(&outside)
    })
}

pub(crate) fn is_page_marker(raw: &str) -> bool {
    raw.to_ascii_lowercase().contains("<page_number>") || footer_label(raw).is_some()
}

/// Page labels printed by the manuals: `3`, `1-7`, `B-12`, `INDEX-3`, and
/// Roman front-matter numbers.
const PAGE_LABEL: &str = r"(?:\d{1,4}-\d{1,4}|[A-Z]{1,5}-\d{1,4}|\d{1,4}|[ivxlcdm]{1,8})";
/// Volume designators that accompany page labels in Intel footers (`Vol. 3B`).
const VOLUME: &str = r"Vol\.\s*(?:\d[A-D]\s*|\d\s+)";

/// A page label surrounded only by optional volume designators.
fn page_label(text: &str) -> Option<String> {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"^\s*(?:{VOLUME})?(?P<label>{PAGE_LABEL})\s*(?:Vol\.\s*\d[A-D]?)?\s*$"
        ))
        .expect("page-label regex must compile")
    });
    let text = collapse_whitespace(text);
    LABEL
        .captures(&text)
        .and_then(|captures| captures.name("label"))
        .map(|label| label.as_str().to_owned())
}

/// Intel footers without a page-number element: `Vol. 1 1-7`, `11-6 Vol. 1`,
/// or an instruction running header followed by `Vol. 2C 5-123`.
fn footer_label(raw: &str) -> Option<String> {
    static FOOTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"^\s*(?:(?P<leading>{PAGE_LABEL})\s*Vol\.\s*\d[A-D]?|(?:[^\n]{{0,200}}\s)?{VOLUME}(?P<trailing>{PAGE_LABEL}))\s*$"
        ))
        .expect("footer regex must compile")
    });
    let captures = FOOTER.captures(raw.trim_end_matches(['\r', '\n']))?;
    captures
        .name("leading")
        .or_else(|| captures.name("trailing"))
        .map(|label| label.as_str().to_owned())
}

/// AMD running headers and classification banners, e.g.
/// `AMD64 Technology 24592—Rev. 3.24—August 2025` or `AMD [AMD Public Use]`.
fn is_running_header(normalized: &str) -> bool {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:(?:AMD64 Technology|\[AMD Public Use\]|AMD|\d{5}\s*[—–-]\s*Rev\.\s*\d+(?:\.\d+)*\s*[—–-]\s*[A-Z][a-z]+\s+\d{4})\s*)+$",
        )
        .expect("running-header regex must compile")
    });
    HEADER.is_match(normalized.trim())
}

/// The title of a `CHAPTER n …` or `APPENDIX X …` heading (empty when the
/// conversion split the title into the following heading).
pub(crate) fn chapter_title(heading: &str) -> Option<String> {
    static CHAPTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:(?i:chapter)\s*\d+|(?i:appendix)\s+[A-Z]\b)\s*(?P<title>.*)$")
            .expect("chapter-heading regex must compile")
    });
    CHAPTER
        .captures(heading)
        .and_then(|captures| captures.name("title"))
        .map(|title| title.as_str().trim().to_owned())
}

/// Intel repeats the chapter title at the top of every page; some conversions
/// turned that running header into a heading.
fn repeats_chapter_title(normalized: &str, chapter_title: &str) -> bool {
    let heading = normalized.trim();
    if !is_uppercase_text(heading) || starts_with_section_number(heading) {
        return false;
    }
    let compact = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    };
    let title = compact(chapter_title);
    !title.is_empty() && compact(heading) == title
}

fn starts_with_section_number(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
}

fn is_uppercase_text(value: &str) -> bool {
    value.chars().any(char::is_alphabetic)
        && value
            .chars()
            .filter(|character| character.is_alphabetic())
            .all(char::is_uppercase)
}

fn is_page_furniture(normalized: &str, known_headings: &[String]) -> bool {
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return false;
    }
    if is_running_header(trimmed) {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "intel logo"
        || lower == "amd logo"
        || (trimmed.len() < 48 && (lower.starts_with("vol. ") || lower.starts_with("volume ")))
    {
        return true;
    }
    if trimmed.len() <= 120 && is_uppercase_text(trimmed) {
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

#[cfg(test)]
mod tests {
    use crate::domain::block::ContentClass;

    use super::{
        HeadingDisposition, classify_body, classify_heading, extract_printed_page, is_page_marker,
    };

    #[test]
    fn page_labels_accept_plain_roman_and_appendix_numbers() {
        for (raw, label) in [
            ("<page_number>2</page_number>", "2"),
            ("<page_number>\n\n142\n</page_number>", "142"),
            ("Vol. 1 <page_number>xvii</page_number>", "xvii"),
            ("<page_number>B-12</page_number>", "B-12"),
            ("<page_number>Vol. 3C 17-35</page_number>", "17-35"),
            ("1-4<page_number>Vol. 1</page_number>", "1-4"),
            ("<page_number>16</page_number> Memory Model", "16"),
            ("Vol. 1 1-7", "1-7"),
            ("11-6 Vol. 1", "11-6"),
            ("A-1 Vol. 3D", "A-1"),
            ("Vol. 3D INDEX-3", "INDEX-3"),
            ("xxviii Vol. 3A", "xxviii"),
            ("Vol. 2B4-187", "4-187"),
            ("XEND—Transactional End Vol. 2D 6-35", "6-35"),
        ] {
            assert_eq!(extract_printed_page(raw).as_deref(), Some(label), "{raw}");
            assert!(is_page_marker(raw), "{raw}");
        }
    }

    #[test]
    fn prose_mentioning_volumes_and_figures_is_not_a_footer() {
        for raw in [
            "*   *Indexed Register-Indirect Addresses*—Figure 2-7 shows the components; see Volume 2.",
            "For more on using the CPUID instruction, see Appendix E of Volume 1.",
            "See Section 13.2 of the Intel 64 and IA-32 Architectures Manual, Vol. 3A, for details.",
        ] {
            assert!(!is_page_marker(raw), "{raw}");
            assert_eq!(extract_printed_page(raw), None, "{raw}");
        }
    }

    #[test]
    fn running_headers_are_page_furniture() {
        for heading in [
            "AMD64 Technology 24592—Rev. 3.24—August 2025",
            "AMD64 Technology",
            "AMD64 Technology **24593—Rev. 3.44—March 2026**",
            "26568—Rev. 3.26—January 2026 AMD64 Technology",
        ] {
            assert_eq!(
                classify_heading(heading, false, None),
                HeadingDisposition::PageFurniture,
                "{heading}"
            );
        }
        assert_eq!(
            classify_heading(
                "PROCESSOR IDENTIFICATION AND FEATURE DETERMINATION",
                false,
                Some("PROCESSOR IDENTIFICATION AND FEATURE DETERMINATION"),
            ),
            HeadingDisposition::PageFurniture
        );
        assert_eq!(
            classify_heading("21.1 USING THE CPUID INSTRUCTION", false, Some("CPUID")),
            HeadingDisposition::Section
        );
        assert_eq!(
            classify_heading("1.1 AMD64 Technology Overview", false, None),
            HeadingDisposition::Section
        );
        assert_eq!(
            classify_body(
                "[AMD Public Use] AMD64 Technology",
                ContentClass::Substantive,
                &[]
            ),
            ContentClass::PageFurniture
        );
    }
}
