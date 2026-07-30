use x86mcp::{
    config::AppConfig,
    corpus::{CorpusManifest, CorpusReader},
    domain::document::ArchiveDocument,
    ingest::parse_document,
};

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
