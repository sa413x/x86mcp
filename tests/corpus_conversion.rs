use x86mcp::{
    config::AppConfig,
    corpus::{CorpusManifest, CorpusReader},
    domain::{block::ContentClass, document::ArchiveDocument},
    ingest::{ParsedDocument, parse_document},
};

const AMD_VOL_1: &str = "amd64-apm-vol-01-application-programming-rev-3.24-2025-08.md";
const INTEL_VOL_1: &str = "intel-sdm-vol-01-basic-architecture-rev-092-2026-06.md";

fn production_document(entry_path: &str) -> ArchiveDocument {
    let config = AppConfig::from_root(env!("CARGO_MANIFEST_DIR")).unwrap();
    let manifest = CorpusManifest::load(&config.corpus_manifest).unwrap();
    CorpusReader::new(manifest, config.corpus_dir)
        .read_all()
        .unwrap()
        .into_iter()
        .find(|document| document.meta.entry_path == entry_path)
        .unwrap_or_else(|| panic!("missing production document {entry_path}"))
}

fn parsed_production_document(entry_path: &str) -> ParsedDocument {
    parse_document(&production_document(entry_path)).unwrap()
}

fn section_page<'a>(parsed: &'a ParsedDocument, heading: &str) -> Option<&'a str> {
    parsed
        .sections
        .iter()
        .find(|section| section.heading == heading)
        .unwrap_or_else(|| panic!("missing section {heading}"))
        .printed_page
        .as_deref()
}

#[test]
fn amd_branch_removal_and_floating_point_figures_are_separate() {
    let document =
        production_document("amd64-apm-vol-01-application-programming-rev-3.24-2025-08.md");
    let parsed = parse_document(&document).unwrap();
    let branch_removal = parsed
        .diagrams
        .iter()
        .find(|diagram| diagram.raw_source.contains("O1[\"operand 163"))
        .expect("branch-removal diagram");
    let floating_point = parsed
        .diagrams
        .iter()
        .find(|diagram| diagram.raw_source.contains("R1H[FP single]"))
        .expect("floating-point diagram");

    assert_ne!(
        branch_removal.source_block_id, floating_point.source_block_id,
        "a converted </mermaid> tag merged Figure 5-5 with Figure 5-6"
    );
}

#[test]
fn intel_processor_trace_fences_preserve_late_sections() {
    let document =
        production_document("intel-sdm-vol-03c-system-programming-guide-part-3-rev-092-2026-06.md");
    let parsed = parse_document(&document).unwrap();

    assert!(
        parsed
            .warnings
            .iter()
            .all(|warning| warning.code != "unclosed_fence"),
        "stray TSV fences leave the end of Intel Volume 3C inside a code block"
    );
    assert!(
        parsed
            .sections
            .iter()
            .any(|section| { section.heading == "36.9.3.4 Calculating Frequency with Intel PT" })
    );
}

#[test]
fn amd_running_headers_stay_inside_the_current_section() {
    let parsed = parsed_production_document(AMD_VOL_1);
    assert!(
        parsed
            .sections
            .iter()
            .all(|section| !section.heading.contains("AMD64 Technology")),
        "AMD running headers became sections"
    );
    let header = parsed
        .blocks
        .iter()
        .find(|block| block.span.line_start == 1861)
        .expect("running header after the first page of Chapter 1");
    assert!(header.raw_source.starts_with("# AMD64 Technology 24592"));
    assert_eq!(header.content_class, ContentClass::PageFurniture);
    assert_eq!(
        header.heading_path.last().map(String::as_str),
        Some("1.1.1 AMD64 Features")
    );
    let register_table = parsed
        .blocks
        .iter()
        .find(|block| block.span.line_start > 1861)
        .unwrap();
    assert_eq!(register_table.content_class, ContentClass::Substantive);
    assert_eq!(
        register_table.heading_path.first().map(String::as_str),
        Some("1 Overview of the AMD64 Architecture")
    );
}

#[test]
fn amd_table_1_1_keeps_every_column_of_its_two_row_header() {
    let parsed = parsed_production_document(AMD_VOL_1);
    let table = parsed
        .tables
        .iter()
        .find(|table| table.raw_source.contains("Application Recompile Required"))
        .expect("Table 1-1");
    assert_eq!(table.caption.as_deref(), Some("Table 1-1. Operating Modes"));
    assert_eq!(table.headers.len(), 10);
    assert!(
        table
            .headers
            .iter()
            .any(|header| header == "Typical / Address Size (bits)")
    );
    assert!(table.rows.iter().all(|row| row.len() == 10));
    assert_eq!(
        table.rows[0][..8],
        [
            "Long Mode",
            "64-Bit Mode",
            "64-bit OS",
            "yes",
            "64",
            "32",
            "yes",
            "64"
        ]
    );
}

#[test]
fn amd_inferred_table_header_is_not_repeated_as_data() {
    let parsed = parsed_production_document(AMD_VOL_1);
    let legend = parsed
        .tables
        .iter()
        .find(|table| table.headers == ["Legend", "Description", "Legend", "Description"])
        .expect("Figure 4-13 legend table");
    assert!(legend.rows.iter().all(|row| row != &legend.headers));
    assert_eq!(legend.rows[0][0], "Bits copied icon");
}

#[test]
fn amd_figure_caption_in_bold_names_its_diagram() {
    let parsed = parsed_production_document(AMD_VOL_1);
    let diagram = parsed
        .diagrams
        .iter()
        .find(|diagram| {
            diagram.caption.as_deref()
                == Some("Figure 2-7. Complex Address Calculation (Protected Mode)")
        })
        .expect("Figure 2-7 diagram");
    assert!(parsed.references.iter().any(|reference| {
        reference.normalized_key == "figure:2-7"
            && reference.target_id.as_deref() == Some(diagram.diagram_id.as_str())
    }));
}

#[test]
fn amd_prose_mentioning_a_volume_is_not_a_page_footer() {
    let parsed = parsed_production_document(AMD_VOL_1);
    let paragraph = parsed
        .blocks
        .iter()
        .find(|block| {
            block
                .normalized_text
                .starts_with("Indexed Register-Indirect Addresses")
        })
        .expect("indexed addressing paragraph");
    assert_eq!(paragraph.content_class, ContentClass::Substantive);
    assert_eq!(paragraph.span.printed_page.as_deref(), Some("16"));
}

#[test]
fn amd_mermaid_chains_and_fan_in_produce_every_edge() {
    let parsed = parsed_production_document(AMD_VOL_1);
    let edges_of = |marker: &str| {
        let diagram = parsed
            .diagrams
            .iter()
            .find(|diagram| diagram.raw_source.contains(marker))
            .unwrap_or_else(|| panic!("missing diagram with {marker}"));
        diagram
            .edges
            .iter()
            .map(|edge| (edge.from.clone(), edge.to.clone()))
            .collect::<Vec<_>>()
    };
    let pair = |from: &str, to: &str| (from.to_owned(), to.to_owned());

    let chain = edges_of("A_OLD_TOP --- A_DATA");
    for expected in [
        pair("A_OLD_TOP", "A_DATA"),
        pair("A_DATA", "A_NEW_TOP"),
        pair("A_NEW_TOP", "A_BASE"),
    ] {
        assert!(chain.contains(&expected), "missing chain edge {expected:?}");
    }
    let fan_in = edges_of("PM_S & PM_EA --> PM_Seg");
    for expected in [pair("PM_S", "PM_Seg"), pair("PM_EA", "PM_Seg")] {
        assert!(
            fan_in.contains(&expected),
            "missing fan-in edge {expected:?}"
        );
    }
}

#[test]
fn page_labels_come_from_the_footer_that_ends_each_page() {
    let intel = parsed_production_document(INTEL_VOL_1);
    assert_eq!(
        section_page(&intel, "1.3.2 Reserved Bits and Software Compatibility"),
        Some("1-7")
    );
    assert_eq!(
        section_page(&intel, "1.3.1 Bit and Byte Order"),
        Some("1-6")
    );

    let amd = parsed_production_document(AMD_VOL_1);
    assert_eq!(section_page(&amd, "1.1 Introduction"), Some("1"));
    assert_eq!(section_page(&amd, "1.1.2 Registers"), Some("3"));
}

#[test]
fn intel_headerless_instruction_summary_table_is_extracted() {
    let parsed = parsed_production_document(INTEL_VOL_1);
    let table = parsed
        .tables
        .iter()
        .find(|table| table.rows.first().is_some_and(|row| row[0] == "JNO"))
        .expect("JNO/JS/JNS table");
    assert!(table.headers.iter().all(String::is_empty));
    assert_eq!(table.rows[1], ["JS", "Jump if sign (negative)."]);
    assert_eq!(table.rows[2], ["JNS", "Jump if not sign (non-negative)."]);
    assert!(
        parsed
            .warnings
            .iter()
            .all(|warning| warning.code != "table_extraction")
    );
}
