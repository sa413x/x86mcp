use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};

use super::tokenizer::X86_TOKENIZER;
pub(crate) const LEXICAL_SCHEMA_VERSION: u32 = 1;

pub(crate) const FIELD_CHUNK_ID: &str = "chunk_id";
pub(crate) const FIELD_VENDOR: &str = "vendor";
pub(crate) const FIELD_DOCUMENT_ID: &str = "document_id";
pub(crate) const FIELD_KIND: &str = "kind";
pub(crate) const FIELD_HEADING: &str = "heading";
pub(crate) const FIELD_CAPTION: &str = "caption";
pub(crate) const FIELD_BODY: &str = "body";
pub(crate) const FIELD_CODE: &str = "code";
pub(crate) const FIELD_SYMBOL: &str = "symbol";
pub(crate) const FIELD_FRONT_MATTER_WEIGHT: &str = "front_matter_weight";
pub(crate) const FIELD_PRINTED_PAGE: &str = "printed_page";
pub(crate) const FIELD_BYTE_START: &str = "byte_start";
pub(crate) const FIELD_BYTE_END: &str = "byte_end";
pub(crate) const FIELD_LINE_START: &str = "line_start";
pub(crate) const FIELD_LINE_END: &str = "line_end";

#[derive(Clone, Copy)]
pub(crate) struct LexicalFields {
    pub chunk_id: Field,
    pub vendor: Field,
    pub document_id: Field,
    pub kind: Field,
    pub heading: Field,
    pub caption: Field,
    pub body: Field,
    pub code: Field,
    pub symbol: Field,
    pub front_matter_weight: Field,
    pub printed_page: Field,
    pub byte_start: Field,
    pub byte_end: Field,
    pub line_start: Field,
    pub line_end: Field,
}

pub(crate) fn build_schema() -> (Schema, LexicalFields) {
    let mut builder = Schema::builder();
    let filter = STRING | FAST;
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(X86_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let fields = LexicalFields {
        chunk_id: builder.add_text_field(FIELD_CHUNK_ID, STRING | STORED),
        vendor: builder.add_text_field(FIELD_VENDOR, filter.clone()),
        document_id: builder.add_text_field(FIELD_DOCUMENT_ID, filter.clone()),
        kind: builder.add_text_field(FIELD_KIND, filter),
        heading: builder.add_text_field(FIELD_HEADING, text.clone()),
        caption: builder.add_text_field(FIELD_CAPTION, text.clone()),
        body: builder.add_text_field(FIELD_BODY, text.clone()),
        code: builder.add_text_field(FIELD_CODE, text),
        symbol: builder.add_text_field(FIELD_SYMBOL, STRING),
        front_matter_weight: builder.add_f64_field(FIELD_FRONT_MATTER_WEIGHT, FAST | STORED),
        printed_page: builder.add_text_field(FIELD_PRINTED_PAGE, STORED),
        byte_start: builder.add_u64_field(FIELD_BYTE_START, FAST | STORED),
        byte_end: builder.add_u64_field(FIELD_BYTE_END, FAST | STORED),
        line_start: builder.add_u64_field(FIELD_LINE_START, FAST | STORED),
        line_end: builder.add_u64_field(FIELD_LINE_END, FAST | STORED),
    };
    (builder.build(), fields)
}

impl LexicalFields {
    pub fn from_schema(schema: &Schema) -> tantivy::Result<Self> {
        Ok(Self {
            chunk_id: schema.get_field(FIELD_CHUNK_ID)?,
            vendor: schema.get_field(FIELD_VENDOR)?,
            document_id: schema.get_field(FIELD_DOCUMENT_ID)?,
            kind: schema.get_field(FIELD_KIND)?,
            heading: schema.get_field(FIELD_HEADING)?,
            caption: schema.get_field(FIELD_CAPTION)?,
            body: schema.get_field(FIELD_BODY)?,
            code: schema.get_field(FIELD_CODE)?,
            symbol: schema.get_field(FIELD_SYMBOL)?,
            front_matter_weight: schema.get_field(FIELD_FRONT_MATTER_WEIGHT)?,
            printed_page: schema.get_field(FIELD_PRINTED_PAGE)?,
            byte_start: schema.get_field(FIELD_BYTE_START)?,
            byte_end: schema.get_field(FIELD_BYTE_END)?,
            line_start: schema.get_field(FIELD_LINE_START)?,
            line_end: schema.get_field(FIELD_LINE_END)?,
        })
    }
}
