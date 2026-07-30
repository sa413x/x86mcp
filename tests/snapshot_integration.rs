use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::Result;
use sha2::{Digest, Sha256};
use x86mcp::{
    config::AppConfig,
    index::{Embedder, FastEmbedder, ModelArtifact, ModelSpec},
    snapshot::{
        SNAPSHOT_SCHEMA_VERSION, Snapshot, SnapshotBuilder, SnapshotError, SnapshotManifest,
        SnapshotPublisher, SnapshotState, snapshot_status,
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

    assert_eq!(manifest["ingest_schema_version"].as_u64(), Some(2));
    manifest["ingest_schema_version"] = serde_json::Value::from(1);
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

struct FakeEmbedder {
    spec: ModelSpec,
    calls: AtomicUsize,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self {
            spec: FastEmbedder::production_spec().unwrap(),
            calls: AtomicUsize::new(0),
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
        Ok(Vec::new())
    }
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
