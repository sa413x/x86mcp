use std::collections::HashSet;

use super::CorpusError;

pub(crate) fn normalize_markdown_path(
    archive_id: &str,
    raw_path: &str,
    seen: &mut HashSet<String>,
) -> Result<String, CorpusError> {
    let slash_normalized = raw_path.replace('\\', "/");
    if slash_normalized.is_empty()
        || slash_normalized.starts_with('/')
        || slash_normalized.contains('\0')
    {
        return Err(CorpusError::UnsafePath {
            archive_id: archive_id.to_owned(),
            entry_path: raw_path.to_owned(),
        });
    }
    let mut components = Vec::new();
    for component in slash_normalized.split('/') {
        if component == "." {
            continue;
        }
        if component.is_empty() || component == ".." || component.ends_with(':') {
            return Err(CorpusError::UnsafePath {
                archive_id: archive_id.to_owned(),
                entry_path: raw_path.to_owned(),
            });
        }
        components.push(component);
    }
    if components.is_empty() {
        return Err(CorpusError::UnsafePath {
            archive_id: archive_id.to_owned(),
            entry_path: raw_path.to_owned(),
        });
    }
    let normalized = components.join("/");
    if !normalized.to_ascii_lowercase().ends_with(".md") {
        return Err(CorpusError::UnsupportedEntry {
            archive_id: archive_id.to_owned(),
            entry_path: normalized,
        });
    }
    if !seen.insert(normalized.clone()) {
        return Err(CorpusError::DuplicatePath {
            archive_id: archive_id.to_owned(),
            entry_path: normalized,
        });
    }
    Ok(normalized)
}
