use std::{collections::HashSet, path::Path};

use tantivy::{
    Index, IndexReader, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, TermQuery},
    schema::{IndexRecordOption, Value},
};

use super::{
    IndexError, LexicalHit, LexicalSearchRequest,
    lexical_writer::{kind_key, vendor_key},
    schema::LexicalFields,
    tokenizer::{normalize_symbol, query_words, register, symbol_terms},
};

pub struct LexicalSearcher {
    reader: IndexReader,
    fields: LexicalFields,
}

impl LexicalSearcher {
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        let index = Index::open_in_dir(path)?;
        register(&index)?;
        let fields = LexicalFields::from_schema(&index.schema())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self { reader, fields })
    }

    pub fn search(&self, request: &LexicalSearchRequest) -> Result<Vec<LexicalHit>, IndexError> {
        if !(1..=100).contains(&request.limit) {
            return Err(IndexError::InvalidRequest(
                "limit must be between 1 and 100".into(),
            ));
        }
        let query = self.query(request)?;
        let searcher = self.reader.searcher();
        let fetch_limit = (request.limit as usize).saturating_mul(4).min(400);
        let candidates = searcher.search(
            query.as_ref(),
            &TopDocs::with_limit(fetch_limit).order_by_score(),
        )?;
        let mut hits = Vec::with_capacity(candidates.len());
        for (score, address) in candidates {
            let document = searcher.doc::<TantivyDocument>(address)?;
            let chunk_id = document
                .get_first(self.fields.chunk_id)
                .and_then(|value| value.as_str())
                .ok_or_else(|| IndexError::Corrupt("stored chunk_id is missing".into()))?;
            let weight = document
                .get_first(self.fields.front_matter_weight)
                .and_then(|value| value.as_f64())
                .ok_or_else(|| {
                    IndexError::Corrupt("stored front_matter_weight is missing".into())
                })? as f32;
            hits.push(LexicalHit {
                chunk_id: chunk_id.to_owned(),
                score: score * weight,
            });
        }
        hits.sort_unstable_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        hits.truncate(request.limit as usize);
        Ok(hits)
    }

    pub fn document_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn query(&self, request: &LexicalSearchRequest) -> Result<Box<dyn Query>, IndexError> {
        let mut relevance = Vec::<Box<dyn Query>>::new();
        let mut seen_words = HashSet::new();
        let mut normalized_words = Vec::new();
        for raw_word in &request.words {
            for word in query_words(raw_word)? {
                if seen_words.insert(word.clone()) {
                    self.add_text_queries(&mut relevance, &word);
                    normalized_words.push(word);
                }
            }
        }
        if normalized_words.len() >= 2 {
            self.add_phrase_queries(&mut relevance, &normalized_words);
        }

        let mut seen_symbols = HashSet::new();
        for raw_symbol in &request.exact_symbols {
            let normalized = normalize_symbol(raw_symbol);
            if normalized.is_empty() || !seen_symbols.insert(normalized.clone()) {
                continue;
            }
            relevance.push(boosted_term(self.fields.symbol, &normalized, 8.0));
            for component in symbol_terms(&normalized).into_iter().skip(1) {
                relevance.push(boosted_term(self.fields.symbol, &component, 2.0));
            }
        }
        if relevance.is_empty() {
            return Err(IndexError::InvalidRequest(
                "request must contain words or exact symbols".into(),
            ));
        }

        let mut clauses = vec![(
            Occur::Must,
            Box::new(BooleanQuery::union(relevance)) as Box<dyn Query>,
        )];
        if let Some(vendor) = request.vendor {
            clauses.push((
                Occur::Must,
                exact_term(self.fields.vendor, vendor_key(vendor)),
            ));
        }
        if let Some(document_id) = request.document_id.as_deref() {
            clauses.push((
                Occur::Must,
                exact_term(self.fields.document_id, document_id),
            ));
        }
        if let Some(kind) = request.kind {
            clauses.push((Occur::Must, exact_term(self.fields.kind, kind_key(kind))));
        }
        if clauses.len() == 1 {
            Ok(clauses.pop().expect("one relevance clause").1)
        } else {
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
    }

    fn add_text_queries(&self, output: &mut Vec<Box<dyn Query>>, word: &str) {
        output.push(boosted_term(self.fields.heading, word, 3.0));
        output.push(boosted_term(self.fields.caption, word, 2.5));
        output.push(boosted_term(self.fields.body, word, 1.0));
        output.push(boosted_term(self.fields.code, word, 1.2));
    }

    fn add_phrase_queries(&self, output: &mut Vec<Box<dyn Query>>, words: &[String]) {
        for (field, boost) in [
            (self.fields.heading, 4.5),
            (self.fields.caption, 3.5),
            (self.fields.body, 1.5),
            (self.fields.code, 1.7),
        ] {
            let terms = words
                .iter()
                .map(|word| Term::from_field_text(field, word))
                .collect();
            output.push(Box::new(BoostQuery::new(
                Box::new(PhraseQuery::new(terms)),
                boost,
            )));
        }
    }
}

fn exact_term(field: tantivy::schema::Field, value: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    ))
}

fn boosted_term(field: tantivy::schema::Field, value: &str, boost: f32) -> Box<dyn Query> {
    Box::new(BoostQuery::new(exact_term(field, value), boost))
}
