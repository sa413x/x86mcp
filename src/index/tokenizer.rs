use std::collections::HashSet;

use tantivy::{
    Index,
    tokenizer::{Language, LowerCaser, RegexTokenizer, Stemmer, TextAnalyzer, TokenStream},
};

pub(crate) const X86_TOKENIZER: &str = "x86_text";
const TOKEN_PATTERN: &str = r"[\p{L}\p{N}#][\p{L}\p{N}#._:/\[\]]*";

pub(crate) fn register(index: &Index) -> tantivy::Result<()> {
    index.tokenizers().register(X86_TOKENIZER, analyzer()?);
    Ok(())
}

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
    let bounded = value.trim_matches(|character: char| !is_symbol_character(character));
    bounded
        .trim_matches(['.', '_', ':', '/', '-'])
        .chars()
        .map(|character| character.to_ascii_uppercase())
        .collect()
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

pub(crate) fn exact_symbols(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(normalize_symbol)
        .filter(|candidate| looks_like_x86_symbol(candidate))
        .fold(
            (Vec::new(), HashSet::new()),
            |(mut output, mut seen), symbol| {
                if seen.insert(symbol.clone()) {
                    output.push(symbol);
                }
                (output, seen)
            },
        )
        .0
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

fn push_unique(output: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if seen.insert(value.clone()) {
        output.push(value);
    }
}
