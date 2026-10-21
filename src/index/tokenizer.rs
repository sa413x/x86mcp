use std::collections::HashSet;

use tantivy::{
    Index,
    tokenizer::{Language, LowerCaser, RegexTokenizer, Stemmer, TextAnalyzer, TokenStream},
};

pub(crate) const X86_TOKENIZER: &str = "x86_text";
/// A token is a run of letters, digits and `#`, optionally joined to further runs by internal
/// x86 punctuation (`CPUID.07H.00H:EBX`, `IA32_EFER`, `CR0.PG`) or followed by bracketed bit
/// ranges (`EBX[5]`, `RFLAGS[11:8]`). Separators must be followed by another run, so
/// sentence-final punctuation (`paging.`, `ampersand:`) never becomes part of a token.
const TOKEN_PATTERN: &str = r"[\p{L}\p{N}#]+(?:[._:/]+[\p{L}\p{N}#]+|\[[\p{L}\p{N}#._:/]*\])*";

pub(crate) fn register(index: &Index) -> tantivy::Result<()> {
    index.tokenizers().register(X86_TOKENIZER, analyzer()?);
    Ok(())
}

/// Analyzes free text into the exact terms stored by the index analyzer.
pub(crate) fn query_words(value: &str) -> tantivy::Result<Vec<String>> {
    let mut analyzer = analyzer()?;
    let mut stream = analyzer.token_stream(value);
    let mut words = Vec::new();
    let mut seen = HashSet::new();
    while stream.advance() {
        let word = stream.token().text.clone();
        if seen.insert(word.clone()) {
            words.push(word);
        }
    }
    Ok(words)
}

pub fn normalize_symbol(value: &str) -> String {
    trim_symbol(value)
        .chars()
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

fn trim_symbol(value: &str) -> &str {
    value
        .trim_matches(|character: char| !is_symbol_character(character))
        .trim_matches(['.', '_', ':', '/', '-'])
}

pub fn symbol_terms(value: &str) -> Vec<String> {
    let normalized = normalize_symbol(value);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    push_unique(&mut terms, &mut seen, normalized.clone());
    for component in normalized
        .split(['.', '_', ':', '/', '-', '[', ']'])
        .map(|component| component.trim_start_matches('#'))
        .filter(|component| !component.is_empty())
    {
        push_unique(&mut terms, &mut seen, component.to_owned());
    }
    terms
}

/// Symbols in a free-text (lexical/hybrid) query: tokens shaped like x86 symbols
/// (`CR4.VMXE`, `IA32_EFER`, `#GP`, `CR0`) or written as uppercase identifiers (`VMRUN`, `EFER`).
/// Ordinary lowercase words stay plain words.
pub(crate) fn exact_symbols(value: &str) -> Vec<String> {
    unique_symbols(value.split_whitespace().filter_map(|raw| {
        let written = trim_symbol(raw);
        let normalized = normalize_symbol(raw);
        (looks_like_x86_symbol(&normalized) || is_uppercase_identifier(written))
            .then_some(normalized)
    }))
}

/// Symbols in an exact-mode query: every whitespace-separated token is a symbol candidate.
pub(crate) fn all_symbols(value: &str) -> Vec<String> {
    unique_symbols(
        value
            .split_whitespace()
            .map(normalize_symbol)
            .filter(|symbol| !symbol.is_empty()),
    )
}

fn unique_symbols(symbols: impl Iterator<Item = String>) -> Vec<String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for symbol in symbols {
        push_unique(&mut output, &mut seen, symbol);
    }
    output
}

fn analyzer() -> tantivy::Result<TextAnalyzer> {
    Ok(TextAnalyzer::builder(RegexTokenizer::new(TOKEN_PATTERN)?)
        .filter(LowerCaser)
        .filter(Stemmer::new(Language::English))
        .build())
}

fn is_symbol_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '#' | '.' | '_' | ':' | '/' | '-' | '[' | ']')
}

fn looks_like_x86_symbol(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.is_ascii()
        && (candidate
            .bytes()
            .any(|byte| matches!(byte, b'#' | b'.' | b'_' | b':' | b'[' | b']'))
            || (candidate.bytes().any(|byte| byte.is_ascii_digit())
                && candidate.bytes().any(|byte| byte.is_ascii_alphabetic())))
}

fn is_uppercase_identifier(candidate: &str) -> bool {
    candidate.len() >= 2
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && candidate.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn push_unique(output: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if seen.insert(value.clone()) {
        output.push(value);
    }
}
