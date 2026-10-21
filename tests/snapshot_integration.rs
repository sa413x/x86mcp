use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use sha2::{Digest, Sha256};
use x86mcp::{
    config::AppConfig,
    index::{Embedder, FastEmbedder, ModelArtifact, ModelSpec},
    ingest::INGEST_SCHEMA_VERSION,
    query::{EmbedderFactory, QueryEngine, SearchMode, SearchRequest},
    snapshot::{
        IndexLock, SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotBuilder, SnapshotError,
        SnapshotManifest, SnapshotPublisher, SnapshotState, StatusMonitor, snapshot_status,
    },
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const INTEL_A: &str = r#"# CHAPTER 26 VMX

<page_number>26-1</page_number>

## 26.1 Virtual-Machine Extensions

VMX operation supports virtual-machine extensions. See Table 26-1 and Figure 26-1.

| Bit | Meaning |
| --- | --- |
| 0 | Disabled |
| 1 | Enabled |

Figure 26-1. VMX state flow

```mermaid
graph TD
  Root --> Guest
  Guest --> Root
```

```asm
VMCALL
VMRESUME
```
"#;

const INTEL_B: &str = r#"# CHAPTER 26 VMX

<page_number>26-1</page_number>

## 26.1 Virtual-Machine Extensions

VMX operation supports virtual-machine extensions. See Table 26-1 and Figure 26-1. VM exits transfer control to the monitor.

| Bit | Meaning |
| --- | --- |
| 0 | Disabled |
| 1 | Enabled |

Figure 26-1. VMX state flow

```mermaid
graph TD
  Root --> Guest
  Guest --> Root
```

```asm
VMCALL
VMRESUME
```
"#;

const AMD: &str = r#"# CHAPTER 15 SVM

<page_number>415</page_number>

## 15.1 Secure Virtual Machine

SVM provides VMRUN and VMSAVE instructions. See Table 15-1.

<table>
<thead><tr><th>Instruction</th><th>Purpose</th></tr></thead>
<tbody>
<tr><td>VMRUN</td><td>Enter guest</td></tr>
<tr><td>VMEXIT</td><td>Return to host</td></tr>
</tbody>
</table>
"#;

#[test]
fn publishes_generations_reuses_vectors_and_rejects_corruption() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let corpus_dir = root.join("corpus");
    let index_dir = root.join("index");
    let model_cache_dir = root.join("models");
    fs::create_dir_all(&corpus_dir).unwrap();
    fs::create_dir_all(&model_cache_dir).unwrap();
    let intel_path = corpus_dir.join("intel.zip");
    let amd_path = corpus_dir.join("amd.zip");
    write_zip(&intel_path, "intel.md", INTEL_A.as_bytes());
    write_zip(&amd_path, "amd.md", AMD.as_bytes());
    write_manifest(&corpus_dir, &intel_path, INTEL_A, &amd_path, AMD);
    let config = AppConfig {
        root: root.to_path_buf(),
        corpus_dir: corpus_dir.clone(),
        corpus_manifest: corpus_dir.join("manifest.toml"),
        index_dir: index_dir.clone(),
        model_cache_dir,
        snapshot_cache_dir: root.join("snapshot-cache"),
    };
    let embedder = FakeEmbedder::new();

    let report_a = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    assert!(report_a.built);
    assert_eq!(report_a.reused_embeddings, 0);
    assert_eq!(report_a.reused_parsed_documents, 0);
    assert!(report_a.embedded_embeddings > 0);
    assert_eq!(report_a.counts.archives, 2);
    assert_eq!(report_a.counts.documents, 2);
    assert_eq!(report_a.counts.tables, 2);
    assert_eq!(report_a.counts.diagrams, 1);
    assert_eq!(report_a.counts.chunks, report_a.counts.vectors);
    let generation_a = index_dir.join("snapshots").join(&report_a.snapshot_id);
    assert_eq!(SNAPSHOT_SCHEMA_VERSION, 2);
    assert!(generation_a.join("catalog.sqlite3.zst").is_file());
    assert!(generation_a.join("vectors.f32.zst").is_file());
    assert!(!generation_a.join("catalog.sqlite3").exists());
    assert!(!generation_a.join("vectors.f32").exists());
    let snapshot_a = Snapshot::open(&index_dir, &config.snapshot_cache_dir).unwrap();
    assert_eq!(snapshot_a.manifest.snapshot_id, report_a.snapshot_id);
    let cache_generation = config.snapshot_cache_dir.join(&report_a.snapshot_id);
    assert!(cache_generation.join("catalog.sqlite3").is_file());
    assert!(cache_generation.join("vectors.f32").is_file());

    let noop = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    assert!(!noop.built);
    assert_eq!(noop.snapshot_id, report_a.snapshot_id);
    assert_eq!(noop.embedded_embeddings, 0);
    assert_eq!(noop.reused_parsed_documents, 2);

    write_zip(&intel_path, "intel.md", INTEL_B.as_bytes());
    write_manifest(&corpus_dir, &intel_path, INTEL_B, &amd_path, AMD);
    let report_b = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    assert!(report_b.built);
    assert_ne!(report_b.snapshot_id, report_a.snapshot_id);
    assert!(report_b.reused_embeddings > 0);
    assert_eq!(report_b.reused_parsed_documents, 1);
    assert!(report_b.embedded_embeddings > 0);
    assert_eq!(
        fs::read_to_string(index_dir.join("CURRENT"))
            .unwrap()
            .trim(),
        report_b.snapshot_id
    );

    assert_eq!(snapshot_a.catalog.document_count().unwrap(), 2);
    assert_eq!(snapshot_a.lexical.document_count(), report_a.counts.chunks);
    assert_eq!(snapshot_a.vectors.count(), report_a.counts.vectors);
    let mut query = vec![0.0; snapshot_a.vectors.dimension()];
    query[0] = 1.0;
    assert_eq!(snapshot_a.vectors.top_k(&query, None, 1).unwrap().len(), 1);

    let status = snapshot_status(&config);
    assert_eq!(status.state, SnapshotState::Ready);
    assert_eq!(
        status.snapshot_id.as_deref(),
        Some(report_b.snapshot_id.as_str())
    );
    assert_eq!(status.counts.as_ref(), Some(&report_b.counts));
    assert!(status.reasons.is_empty());
    assert_eq!(status.freshness.len(), 2);
    assert!(status.freshness.iter().all(|archive| archive.fresh));

    fs::write(&amd_path, b"changed archive bytes").unwrap();
    let stale_status = snapshot_status(&config);
    assert_eq!(stale_status.state, SnapshotState::Stale);
    let amd_freshness = stale_status
        .freshness
        .iter()
        .find(|archive| archive.archive_id == "amd")
        .unwrap();
    assert!(!amd_freshness.fresh);
    assert!(amd_freshness.actual_sha256.is_some());

    let corrupt_candidate = index_dir.join(".build-corrupt");
    copy_tree(
        &index_dir.join("snapshots").join(&report_b.snapshot_id),
        &corrupt_candidate,
    );
    let corrupt_manifest =
        SnapshotManifest::load(&corrupt_candidate.join("snapshot.json")).unwrap();
    let vector_path = corrupt_candidate.join(&corrupt_manifest.components.vectors.parts[0].path);
    let vector_len = fs::metadata(&vector_path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&vector_path)
        .unwrap()
        .set_len(vector_len - 4)
        .unwrap();
    let current_before = fs::read_to_string(index_dir.join("CURRENT")).unwrap();
    let corrupt_result = SnapshotPublisher::publish(
        &index_dir,
        &corrupt_candidate,
        &report_b.snapshot_id,
        &config.snapshot_cache_dir,
    );
    assert!(matches!(
        corrupt_result,
        Err(SnapshotError::Invalid(message))
            if message.contains("compressed artifact")
    ));
    assert_eq!(
        fs::read_to_string(index_dir.join("CURRENT")).unwrap(),
        current_before
    );
    let reopened = Snapshot::open(&index_dir, &config.snapshot_cache_dir).unwrap();
    assert_eq!(reopened.manifest.snapshot_id, report_b.snapshot_id);
    assert!(embedder.calls.load(Ordering::Relaxed) >= 2);
}

#[test]
fn ingest_schema_change_invalidates_parsed_document_reuse() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let corpus_dir = root.join("corpus");
    let index_dir = root.join("index");
    fs::create_dir_all(&corpus_dir).unwrap();
    let intel_path = corpus_dir.join("intel.zip");
    let amd_path = corpus_dir.join("amd.zip");
    write_zip(&intel_path, "intel.md", INTEL_A.as_bytes());
    write_zip(&amd_path, "amd.md", AMD.as_bytes());
    write_manifest(&corpus_dir, &intel_path, INTEL_A, &amd_path, AMD);
    let config = AppConfig {
        root: root.to_path_buf(),
        corpus_dir: corpus_dir.clone(),
        corpus_manifest: corpus_dir.join("manifest.toml"),
        index_dir: index_dir.clone(),
        model_cache_dir: root.join("models"),
        snapshot_cache_dir: root.join("snapshot-cache"),
    };
    let embedder = FakeEmbedder::new();
    let first = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    let manifest_path = index_dir
        .join("snapshots")
        .join(&first.snapshot_id)
        .join("snapshot.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();

    assert_eq!(
        manifest["ingest_schema_version"].as_u64(),
        Some(u64::from(INGEST_SCHEMA_VERSION))
    );
    manifest["ingest_schema_version"] = serde_json::Value::from(INGEST_SCHEMA_VERSION - 1);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    write_zip(&intel_path, "intel.md", INTEL_B.as_bytes());
    write_manifest(&corpus_dir, &intel_path, INTEL_B, &amd_path, AMD);

    let rebuilt = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    assert!(rebuilt.built);
    assert_eq!(rebuilt.reused_parsed_documents, 0);
    assert!(rebuilt.reused_embeddings > 0);
}

#[test]
fn index_lock_serializes_builds() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let embedder = FakeEmbedder::new();
    let first = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();

    let held = IndexLock::acquire(&config.index_dir).unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let report = SnapshotBuilder::new(&config, &embedder).build(true);
            sender.send(report).unwrap();
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!has_entry_with_prefix(&config.index_dir, ".build-"));

        drop(held);
        let report = receiver
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert!(report.built);
        assert_eq!(report.snapshot_id, first.snapshot_id);
    });
    Snapshot::open(&config.index_dir, &config.snapshot_cache_dir).unwrap();
}

#[test]
fn vector_reuse_requires_matching_model_artifacts() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let original = FakeEmbedder::with_artifacts(vec![fake_artifact("original weights")]);
    let first = SnapshotBuilder::new(&config, &original)
        .build(false)
        .unwrap();
    assert!(first.embedded_embeddings > 0);

    let retrained = FakeEmbedder::with_artifacts(vec![fake_artifact("retrained weights")]);
    let second = SnapshotBuilder::new(&config, &retrained)
        .build(false)
        .unwrap();
    assert!(second.built);
    assert_ne!(second.snapshot_id, first.snapshot_id);
    assert_eq!(second.reused_embeddings, 0);
    assert_eq!(second.embedded_embeddings, second.counts.vectors);
    assert_eq!(second.reused_parsed_documents, 2);
}

#[test]
fn force_rebuild_reprocesses_everything_and_replaces_generation() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let embedder = FakeEmbedder::new();
    let first = SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    let generation = config.index_dir.join("snapshots").join(&first.snapshot_id);
    fs::write(generation.join("previous-generation-marker"), b"old").unwrap();
    let calls_before = embedder.calls.load(Ordering::Relaxed);

    let forced = SnapshotBuilder::new(&config, &embedder)
        .build(true)
        .unwrap();
    assert!(forced.built);
    assert_eq!(forced.snapshot_id, first.snapshot_id);
    assert_eq!(forced.reused_parsed_documents, 0);
    assert_eq!(forced.reused_embeddings, 0);
    assert_eq!(forced.embedded_embeddings, forced.counts.vectors);
    assert!(embedder.calls.load(Ordering::Relaxed) > calls_before);
    assert!(!generation.join("previous-generation-marker").exists());
    assert!(!has_entry_with_prefix(&config.index_dir, ".replaced-"));
    assert!(!has_entry_with_prefix(&config.index_dir, ".build-"));
    let reopened = Snapshot::open(&config.index_dir, &config.snapshot_cache_dir).unwrap();
    assert_eq!(reopened.manifest.snapshot_id, first.snapshot_id);
}

#[test]
fn semantic_degradation_is_reported_while_stale() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let artifact = install_fake_model(&config, "weights");
    let model_path = config.model_cache_dir.join(&artifact.path);
    let embedder = FakeEmbedder::with_artifacts(vec![artifact.clone()]);
    SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    let ready = snapshot_status(&config);
    assert_eq!(ready.state, SnapshotState::Ready);
    assert!(ready.semantic_degraded_reasons.is_empty());

    fs::remove_file(&model_path).unwrap();
    fs::write(config.corpus_dir.join("amd.zip"), b"changed archive bytes").unwrap();
    let status = snapshot_status(&config);
    assert_eq!(status.state, SnapshotState::Stale);
    assert!(!status.semantic_degraded_reasons.is_empty());

    let engine = fake_engine(&config, vec![artifact]);
    let response = engine.search(search("VMX", SearchMode::Semantic)).unwrap();
    assert!(response.state.stale);
    assert!(response.state.semantic_degraded_reason.is_some());
    assert!(response.hits.is_empty());
    assert!(
        !engine
            .index_status()
            .status
            .semantic_degraded_reasons
            .is_empty()
    );
}

#[test]
fn semantic_search_rejects_embedder_with_different_model_artifacts() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let embedder =
        FakeEmbedder::with_artifacts(vec![install_fake_model(&config, "snapshot weights")]);
    SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();

    let matching = fake_engine(&config, vec![fake_artifact("snapshot weights")]);
    let response = matching
        .search(search("VMX", SearchMode::Semantic))
        .unwrap();
    assert!(response.state.semantic_degraded_reason.is_none());
    assert!(!response.hits.is_empty());

    let mismatched = fake_engine(&config, vec![fake_artifact("other weights")]);
    let response = mismatched
        .search(search("VMX", SearchMode::Semantic))
        .unwrap();
    assert!(response.state.semantic_degraded_reason.is_some());
    assert!(response.hits.is_empty());
}

#[test]
fn query_engine_status_tracks_corpus_changes_after_start() {
    let temporary = tempfile::tempdir().unwrap();
    let config = corpus_config(temporary.path());
    let embedder = FakeEmbedder::new();
    SnapshotBuilder::new(&config, &embedder)
        .build(false)
        .unwrap();
    let engine = fake_engine(&config, Vec::new());
    assert_eq!(engine.index_status().status.state, SnapshotState::Ready);
    let fresh = engine.search(search("VMX", SearchMode::Lexical)).unwrap();
    assert!(!fresh.state.stale);

    write_zip(
        &config.corpus_dir.join("amd.zip"),
        "amd.md",
        INTEL_B.as_bytes(),
    );
    let status = engine.index_status().status;
    assert_eq!(status.state, SnapshotState::Stale);
    assert!(
        status
            .freshness
            .iter()
            .any(|archive| archive.archive_id == "amd" && !archive.fresh)
    );
    let stale = engine.search(search("VMX", SearchMode::Lexical)).unwrap();
    assert!(stale.state.stale);
}

struct FakeEmbedder {
    spec: ModelSpec,
    calls: AtomicUsize,
    artifacts: Vec<ModelArtifact>,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self::with_artifacts(Vec::new())
    }

    fn with_artifacts(artifacts: Vec<ModelArtifact>) -> Self {
        Self {
            spec: FastEmbedder::production_spec().unwrap(),
            calls: AtomicUsize::new(0),
            artifacts,
        }
    }

    fn vector(&self, text: &str) -> Vec<f32> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut vector = vec![0.0; self.spec.dimension];
        vector[0] = 1.0;
        vector[1] = f32::from(bytes[0]) / 255.0;
        vector[2] = f32::from(bytes[1]) / 255.0;
        vector
    }
}

impl Embedder for FakeEmbedder {
    fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(text.split_whitespace().count())
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(texts.iter().map(|text| self.vector(text)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.vector(text))
    }

    fn artifact_hashes(&self) -> Result<Vec<ModelArtifact>> {
        Ok(self.artifacts.clone())
    }
}

fn fake_artifact(weights: &str) -> ModelArtifact {
    ModelArtifact {
        path: "models--fake--model/snapshots/main/model.onnx".into(),
        blake3: blake3::hash(weights.as_bytes()).to_hex().to_string(),
    }
}

fn install_fake_model(config: &AppConfig, weights: &str) -> ModelArtifact {
    let artifact = fake_artifact(weights);
    let path = config.model_cache_dir.join(&artifact.path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, weights).unwrap();
    artifact
}

fn fake_engine(config: &AppConfig, artifacts: Vec<ModelArtifact>) -> QueryEngine {
    let snapshot = Snapshot::open(&config.index_dir, &config.snapshot_cache_dir).unwrap();
    let factory: EmbedderFactory = Arc::new(move || {
        Ok(Arc::new(FakeEmbedder::with_artifacts(artifacts.clone())) as Arc<dyn Embedder>)
    });
    QueryEngine::new(snapshot, StatusMonitor::new(config), Some(factory)).unwrap()
}

fn search(query: &str, mode: SearchMode) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode,
        vendors: Vec::new(),
        document_ids: Vec::new(),
        kinds: Vec::new(),
        limit: 10,
        cursor: None,
    }
}

fn corpus_config(root: &Path) -> AppConfig {
    let corpus_dir = root.join("corpus");
    let model_cache_dir = root.join("models");
    fs::create_dir_all(&corpus_dir).unwrap();
    fs::create_dir_all(&model_cache_dir).unwrap();
    let intel_path = corpus_dir.join("intel.zip");
    let amd_path = corpus_dir.join("amd.zip");
    write_zip(&intel_path, "intel.md", INTEL_A.as_bytes());
    write_zip(&amd_path, "amd.md", AMD.as_bytes());
    write_manifest(&corpus_dir, &intel_path, INTEL_A, &amd_path, AMD);
    AppConfig {
        root: root.to_path_buf(),
        corpus_dir: corpus_dir.clone(),
        corpus_manifest: corpus_dir.join("manifest.toml"),
        index_dir: root.join("index"),
        model_cache_dir,
        snapshot_cache_dir: root.join("snapshot-cache"),
    }
}

fn has_entry_with_prefix(directory: &Path, prefix: &str) -> bool {
    fs::read_dir(directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(prefix)
    })
}

fn write_manifest(corpus_dir: &Path, intel_path: &Path, intel: &str, amd_path: &Path, amd: &str) {
    let intel_hash = sha256(intel_path);
    let amd_hash = sha256(amd_path);
    fs::write(
        corpus_dir.join("manifest.toml"),
        format!(
            "schema_version = 1\n\n[[archives]]\nid = \"intel\"\nvendor = \"intel\"\npath = \"intel.zip\"\nsha256 = \"{intel_hash}\"\nentry_count = 1\nuncompressed_bytes = {}\n\n[[archives]]\nid = \"amd\"\nvendor = \"amd\"\npath = \"amd.zip\"\nsha256 = \"{amd_hash}\"\nentry_count = 1\nuncompressed_bytes = {}\n",
            intel.len(),
            amd.len()
        ),
    )
    .unwrap();
}

fn write_zip(path: &Path, entry: &str, bytes: &[u8]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file(entry, options).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap();
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&source_path, &target_path);
        } else {
            fs::copy(source_path, target_path).unwrap();
        }
    }
}
