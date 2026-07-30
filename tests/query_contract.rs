use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use sha2::{Digest, Sha256};
use x86mcp::{
    catalog::ReferenceDirection,
    config::AppConfig,
    domain::{Vendor, chunk::ChunkKind},
    index::{Embedder, FastEmbedder, LexicalSearchRequest, ModelSpec},
    query::{
        BuildContextRequest, CompareVendorsRequest, EmbedderFactory, EntityKind, EntityState,
        GetDiagramRequest, GetOutlineRequest, GetReferencesRequest, GetSectionRequest,
        GetTableRequest, LookupRequest, QueryEngine, SearchMode, SearchRequest,
    },
    snapshot::{Snapshot, SnapshotBuilder, snapshot_status},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const INTEL: &str = r#"# Intel System Programming Manual

## IA32_EFER Extended Feature Enable Register

IA32_EFER controls extended processor features. EFER is a model-specific register.

## VMX Operation

VMX operation provides Intel virtualization. Set CR4.VMXE to enable virtual-machine extensions and enter a guest. See Table 26-1 and Figure 26-1.

| Bit | Meaning |
| --- | --- |
| 13 | VMXE |
| 0 | Lock |

Table 26-1. VMX controls

```mermaid
graph TD
  Root --> Guest
  Guest --> Root
```

Figure 26-1. VMX state flow
"#;

const AMD: &str = r#"# AMD Architecture Manual

## EFER Register

The AMD EFER register enables long mode and system-call extensions.

## SVM Operation

SVM operation provides AMD virtualization. VMRUN enters a guest and VMEXIT returns to the host.
"#;

#[test]
fn exact_hybrid_and_vendor_filtering_return_cited_evidence() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = fixture.engine(calls.clone());

    let exact = engine
        .search(search("IA32_EFER", SearchMode::Exact))
        .unwrap();
    assert!(!exact.hits.is_empty());
    assert!(exact.hits[0].snippet.contains("IA32_EFER"));
    assert_eq!(exact.hits[0].citation.vendor, Vendor::Intel);
    assert!(!exact.hits[0].citation.document_id.is_empty());
    assert!(!exact.hits[0].citation.entry_path.is_empty());
    assert!(!exact.hits[0].citation.section_id.is_empty());
    assert!(exact.hits[0].scores.exact_rank.is_some());

    let english = engine
        .search(search("virtual machine extensions VMX", SearchMode::Hybrid))
        .unwrap();
    let russian = engine
        .search(search(
            "расширения виртуальной машины VMX",
            SearchMode::Hybrid,
        ))
        .unwrap();
    assert_eq!(
        english.hits[0].citation.section_id,
        russian.hits[0].citation.section_id
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let mut amd_only = search("EFER register", SearchMode::Lexical);
    amd_only.vendors = vec![Vendor::Amd];
    let filtered = engine.search(amd_only).unwrap();
    assert!(!filtered.hits.is_empty());
    assert!(
        filtered
            .hits
            .iter()
            .all(|hit| hit.citation.vendor == Vendor::Amd)
    );
}

#[test]
fn search_cursor_pages_a_stable_candidate_window_without_duplicates() {
    let fixture = Fixture::new();
    let engine = fixture.engine(Arc::new(AtomicUsize::new(0)));
    let mut request = search("operation", SearchMode::Lexical);
    request.limit = 1;
    let mut seen = HashSet::new();
    let mut pages = 0;

    loop {
        let response = engine.search(request.clone()).unwrap();
        assert!(!response.candidate_window_truncated);
        for hit in response.hits {
            assert!(seen.insert(hit.chunk_id));
        }
        pages += 1;
        assert!(pages <= 20, "cursor did not exhaust the candidate window");
        let Some(cursor) = response.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }

    assert!(seen.len() >= 2);
}

#[test]
fn hybrid_search_ranks_more_than_256_unique_candidates() {
    const QUERY: &str = "IA32_OVERFLOW alpha beta gamma delta epsilon";
    let mut intel = String::from("# Intel Candidate Stress Manual\n\n");
    for index in 0..100 {
        intel.push_str(&format!(
            "## Exact candidate {index}\n\nIA32_OVERFLOW exact-only evidence {index}.\n\n"
        ));
    }
    for index in 0..100 {
        intel.push_str(&format!(
            "## Alpha beta gamma delta epsilon {index}\n\nalpha beta gamma delta epsilon lexical-only evidence {index}.\n\n"
        ));
    }
    for index in 0..100 {
        intel.push_str(&format!(
            "## Semantic candidate {index}\n\nsemantic-only evidence {index}.\n\n"
        ));
    }
    let fixture = Fixture::with_intel(&intel);
    let snapshot = Snapshot::open(
        &fixture.config.index_dir,
        &fixture.config.snapshot_cache_dir,
    )
    .unwrap();
    let lexical_request = LexicalSearchRequest::from_query(QUERY, 100).unwrap();
    let mut exact_request = lexical_request.clone();
    exact_request.words.clear();
    let exact = snapshot.lexical.search(&exact_request).unwrap();
    let lexical = snapshot.lexical.search(&lexical_request).unwrap();
    let query_vector = FixtureEmbedder::new().vector(QUERY);
    let vector_metadata = snapshot.catalog.vector_metadata().unwrap();
    let semantic = snapshot.vectors.top_k(&query_vector, None, 100).unwrap();
    let candidate_ids = exact
        .iter()
        .chain(&lexical)
        .map(|hit| hit.chunk_id.clone())
        .chain(
            semantic
                .iter()
                .map(|hit| vector_metadata[hit.row as usize].chunk_id.clone()),
        )
        .collect::<HashSet<_>>();
    assert!(
        candidate_ids.len() > 256,
        "fixture produced only {} unique candidates",
        candidate_ids.len()
    );

    let engine = QueryEngine::new(
        snapshot,
        snapshot_status(&fixture.config),
        Some(Arc::new(|| Ok(Arc::new(FixtureEmbedder::new())))),
    )
    .unwrap();
    let first = engine.search(search(QUERY, SearchMode::Hybrid)).unwrap();
    let second = engine.search(search(QUERY, SearchMode::Hybrid)).unwrap();
    assert_eq!(first.hits.len(), 10);
    assert_eq!(first.hits, second.hits);
}

#[test]
fn lexical_search_is_model_lazy_and_vendor_comparison_is_balanced() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = fixture.engine(calls.clone());

    let lexical = engine
        .search(search("virtualization operation", SearchMode::Lexical))
        .unwrap();
    assert!(!lexical.hits.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let compared = engine
        .compare_vendors(CompareVendorsRequest {
            query: "virtualization operation".into(),
            mode: SearchMode::Lexical,
            limit_per_vendor: 3,
        })
        .unwrap();
    assert!(!compared.intel.is_empty());
    assert!(!compared.amd.is_empty());
    assert!(
        compared
            .intel
            .iter()
            .all(|hit| hit.citation.vendor == Vendor::Intel)
    );
    assert!(
        compared
            .amd
            .iter()
            .all(|hit| hit.citation.vendor == Vendor::Amd)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn context_deduplicates_ids_and_respects_budget() {
    let fixture = Fixture::new();
    let engine = fixture.engine(Arc::new(AtomicUsize::new(0)));
    let hits = engine
        .search(search("VMX VMXE virtualization", SearchMode::Lexical))
        .unwrap()
        .hits;
    let id = hits[0].chunk_id.clone();
    let context = engine
        .build_context(BuildContextRequest {
            query: None,
            chunk_ids: vec![id.clone(), id],
            mode: SearchMode::Lexical,
            vendors: Vec::new(),
            token_budget: 256,
        })
        .unwrap();
    assert!(context.estimated_tokens <= 256);
    assert_eq!(
        context
            .items
            .iter()
            .flat_map(|item| item.chunk_ids.iter())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1
    );
    assert!(context.items.iter().all(|item| !item.citations.is_empty()));
}

#[test]
fn unknown_typed_id_returns_snapshot_scoped_not_found() {
    let fixture = Fixture::new();
    let engine = fixture.engine(Arc::new(AtomicUsize::new(0)));
    let response = engine
        .get_table(GetTableRequest {
            id: "tbl:missing".into(),
            offset: 0,
            limit: 10,
            row_filter: None,
            include_raw: false,
        })
        .unwrap();
    assert_eq!(response.entity_state, EntityState::NotFound);
    assert_eq!(response.state.snapshot_id, fixture.snapshot_id);
    assert!(response.table.is_none());
}

#[test]
fn typed_catalog_services_return_structured_current_snapshot_data() {
    let fixture = Fixture::new();
    let snapshot = Snapshot::open(
        &fixture.config.index_dir,
        &fixture.config.snapshot_cache_dir,
    )
    .unwrap();
    let intel = snapshot
        .catalog
        .documents()
        .unwrap()
        .into_iter()
        .find(|document| document.meta.vendor == Vendor::Intel)
        .unwrap();
    let parsed = snapshot
        .catalog
        .parsed_document(&intel.meta.document_id)
        .unwrap()
        .unwrap();
    let section_id = parsed.sections[2].section_id.clone();
    let table_id = parsed.tables[0].table_id.clone();
    let diagram_id = parsed.diagrams[0].diagram_id.clone();
    let reference_source = parsed.references[0].source_block_id.clone();
    drop(snapshot);
    let engine = fixture.engine(Arc::new(AtomicUsize::new(0)));

    let lookup = engine
        .lookup(LookupRequest {
            entity: "IA32_EFER".into(),
            kind: EntityKind::Msr,
            vendors: vec![Vendor::Intel],
            limit: 5,
        })
        .unwrap();
    assert_eq!(lookup.entity_state, EntityState::Found);
    assert!(!lookup.exact.is_empty());

    let outline = engine
        .get_outline(GetOutlineRequest {
            document_id: None,
            root_section_id: None,
            depth: 2,
            limit: 20,
            cursor: None,
        })
        .unwrap();
    assert_eq!(outline.documents.len(), 2);

    let section = engine
        .get_section(GetSectionRequest {
            id: section_id,
            block_limit: 20,
            cursor: None,
            include_neighbors: true,
        })
        .unwrap();
    assert_eq!(section.entity_state, EntityState::Found);
    assert!(section.section.is_some());
    assert!(section.citation.is_some());

    let table = engine
        .get_table(GetTableRequest {
            id: table_id,
            offset: 0,
            limit: 10,
            row_filter: Some("VMXE".into()),
            include_raw: false,
        })
        .unwrap();
    assert_eq!(table.table.unwrap().rows, vec![vec!["13", "VMXE"]]);
    assert!(table.citation.is_some());

    let diagram = engine
        .get_diagram(GetDiagramRequest {
            id: diagram_id,
            include_raw: true,
            include_surrounding: true,
        })
        .unwrap();
    assert_eq!(diagram.entity_state, EntityState::Found);
    assert!(!diagram.diagram.unwrap().edges.is_empty());
    assert!(diagram.citation.is_some());

    let references = engine
        .get_references(GetReferencesRequest {
            id: reference_source,
            direction: ReferenceDirection::Outgoing,
            limit: 20,
        })
        .unwrap();
    assert_eq!(references.entity_state, EntityState::Found);
    assert!(!references.references.is_empty());

    let status = engine.index_status();
    assert_eq!(status.manifest.snapshot_id, fixture.snapshot_id);
    assert_eq!(status.counts.documents, 2);
}

fn search(query: &str, mode: SearchMode) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode,
        vendors: Vec::new(),
        document_ids: Vec::new(),
        kinds: Vec::<ChunkKind>::new(),
        limit: 10,
        cursor: None,
    }
}

struct Fixture {
    _temporary: tempfile::TempDir,
    config: AppConfig,
    snapshot_id: String,
}

impl Fixture {
    fn new() -> Self {
        Self::with_intel(INTEL)
    }

    fn with_intel(intel: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let corpus_dir = root.join("corpus");
        let model_cache_dir = root.join("models");
        fs::create_dir_all(&corpus_dir).unwrap();
        fs::create_dir_all(&model_cache_dir).unwrap();
        let intel_path = corpus_dir.join("intel.zip");
        let amd_path = corpus_dir.join("amd.zip");
        write_zip(&intel_path, "intel.md", intel.as_bytes());
        write_zip(&amd_path, "amd.md", AMD.as_bytes());
        write_manifest(&corpus_dir, &intel_path, intel.len(), &amd_path, AMD.len());
        let config = AppConfig {
            root: root.to_path_buf(),
            corpus_dir: corpus_dir.clone(),
            corpus_manifest: corpus_dir.join("manifest.toml"),
            index_dir: root.join("index"),
            model_cache_dir,
            snapshot_cache_dir: root.join("snapshot-cache"),
        };
        let report = SnapshotBuilder::new(&config, &FixtureEmbedder::new())
            .build(false)
            .unwrap();
        Self {
            _temporary: temporary,
            config,
            snapshot_id: report.snapshot_id,
        }
    }

    fn engine(&self, calls: Arc<AtomicUsize>) -> QueryEngine {
        let snapshot =
            Snapshot::open(&self.config.index_dir, &self.config.snapshot_cache_dir).unwrap();
        let status = snapshot_status(&self.config);
        let factory: EmbedderFactory = Arc::new(move || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(FixtureEmbedder::new()))
        });
        QueryEngine::new(snapshot, status, Some(factory)).unwrap()
    }
}

struct FixtureEmbedder {
    spec: ModelSpec,
}

impl FixtureEmbedder {
    fn new() -> Self {
        Self {
            spec: FastEmbedder::production_spec().unwrap(),
        }
    }

    fn vector(&self, text: &str) -> Vec<f32> {
        let text = text.to_lowercase();
        let coordinate = if text.contains("exact-only") || text.contains("lexical-only") {
            5
        } else if text.contains("vmx")
            || text.contains("virtual-machine")
            || text.contains("виртуал")
        {
            1
        } else if text.contains("svm") || text.contains("vmrun") {
            2
        } else if text.contains("efer") {
            3
        } else {
            4
        };
        let mut vector = vec![0.0; self.spec.dimension];
        vector[coordinate] = 1.0;
        vector
    }
}

impl Embedder for FixtureEmbedder {
    fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(text.split_whitespace().count())
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| self.vector(text)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.vector(text))
    }
}

fn write_zip(path: &Path, entry: &str, bytes: &[u8]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file(entry, options).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap();
}

fn write_manifest(
    corpus_dir: &Path,
    intel_path: &Path,
    intel_len: usize,
    amd_path: &Path,
    amd_len: usize,
) {
    let intel_hash = format!("{:x}", Sha256::digest(fs::read(intel_path).unwrap()));
    let amd_hash = format!("{:x}", Sha256::digest(fs::read(amd_path).unwrap()));
    fs::write(
        corpus_dir.join("manifest.toml"),
        format!(
            "schema_version = 1\n\n[[archives]]\nid = \"intel\"\nvendor = \"intel\"\npath = \"intel.zip\"\nsha256 = \"{intel_hash}\"\nentry_count = 1\nuncompressed_bytes = {}\n\n[[archives]]\nid = \"amd\"\nvendor = \"amd\"\npath = \"amd.zip\"\nsha256 = \"{amd_hash}\"\nentry_count = 1\nuncompressed_bytes = {}\n",
            intel_len,
            amd_len
        ),
    )
    .unwrap();
}
