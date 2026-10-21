use std::{
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::TempDir;
use x86mcp::{
    config::AppConfig,
    index::{Embedder, FastEmbedder, ModelSpec},
    setup::{SetupOptions, install_data},
    snapshot::{Snapshot, SnapshotBuilder},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const BROKEN_SNAPSHOT_ID: &str = "fixture-snapshot";
const MARKDOWN: &str = "# CHAPTER 1 BASICS\n\n<page_number>1-1</page_number>\n\n## 1.1 Registers\n\nThe RAX register holds the accumulator. See Table 1-1.\n\n| Register | Width |\n| --- | --- |\n| RAX | 64 |\n";

#[test]
fn installs_a_verified_data_bundle() {
    let bundle = ValidBundle::new();
    let destination = tempfile::tempdir().unwrap();
    let config = isolated_config(destination.path());

    let (report, ()) =
        install_data(&config, &bundle.options(false), || open_installed(&config)).unwrap();

    assert!(report.installed);
    assert_eq!(report.snapshot_id, bundle.snapshot_id);
    assert_eq!(
        fs::read_to_string(destination.path().join("index/CURRENT"))
            .unwrap()
            .trim(),
        bundle.snapshot_id
    );
    assert!(destination.path().join("corpus/manifest.toml").is_file());
    open_installed(&config).unwrap();
}

#[test]
fn broken_snapshot_bundle_preserves_the_existing_installation() {
    let valid = ValidBundle::new();
    let destination = tempfile::tempdir().unwrap();
    let config = isolated_config(destination.path());
    install_data(&config, &valid.options(false), || open_installed(&config)).unwrap();

    let source = tempfile::tempdir().unwrap();
    let broken = source.path().join("x86mcp-data.tar.zst");
    write_shape_only_bundle(&broken, env!("CARGO_PKG_VERSION"), BROKEN_SNAPSHOT_ID);
    let result = install_data(
        &config,
        &SetupOptions {
            data_source: Some(broken.display().to_string()),
            expected_sha256: Some(sha256(&broken)),
            force: true,
        },
        // Accept anything after publishing: the staged snapshot check alone
        // must keep the broken bundle out.
        || Ok(()),
    );

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(destination.path().join("index/CURRENT"))
            .unwrap()
            .trim(),
        valid.snapshot_id
    );
    open_installed(&config).unwrap();
}

#[test]
fn failed_verification_restores_the_previous_installation() {
    let bundle = ValidBundle::new();
    let destination = tempfile::tempdir().unwrap();
    let config = isolated_config(destination.path());
    install_data(&config, &bundle.options(false), || open_installed(&config)).unwrap();
    let sentinel = destination.path().join("corpus/previous-install");
    fs::write(&sentinel, "keep").unwrap();

    let result = install_data(&config, &bundle.options(true), || -> Result<()> {
        open_installed(&config)?;
        assert!(!sentinel.exists(), "the new installation must be published");
        bail!("model download failed")
    });

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    open_installed(&config).unwrap();
    let leftovers: Vec<_> = fs::read_dir(destination.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(".x86mcp-setup-"))
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

#[test]
fn failed_first_verification_leaves_no_partial_installation() {
    let bundle = ValidBundle::new();
    let destination = tempfile::tempdir().unwrap();
    let config = isolated_config(destination.path());

    let result = install_data(&config, &bundle.options(false), || -> Result<()> {
        bail!("model download failed")
    });

    assert!(result.is_err());
    assert!(!destination.path().join("index").exists());
    assert!(!destination.path().join("corpus").exists());
    assert!(!destination.path().join("x86mcp-data.json").exists());
}

#[test]
fn checksum_failure_preserves_the_existing_installation() {
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("x86mcp-data.tar.zst");
    write_shape_only_bundle(&archive, env!("CARGO_PKG_VERSION"), BROKEN_SNAPSHOT_ID);

    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("corpus")).unwrap();
    fs::write(destination.path().join("corpus/sentinel"), "keep").unwrap();
    let config = isolated_config(destination.path());
    let error = install_data(
        &config,
        &SetupOptions {
            data_source: Some(archive.display().to_string()),
            expected_sha256: Some("0".repeat(64)),
            force: true,
        },
        || Ok(()),
    )
    .unwrap_err();

    assert!(error.to_string().contains("checksum mismatch"));
    assert_eq!(
        fs::read_to_string(destination.path().join("corpus/sentinel")).unwrap(),
        "keep"
    );
    assert!(!destination.path().join("index").exists());
}

#[test]
fn rejects_archive_path_traversal() {
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("malicious-data.tar.zst");
    write_path_traversal_bundle(&archive);

    let destination = tempfile::tempdir().unwrap();
    let config = isolated_config(destination.path());
    let error = install_data(
        &config,
        &SetupOptions {
            data_source: Some(archive.display().to_string()),
            expected_sha256: Some(sha256(&archive)),
            force: true,
        },
        || Ok(()),
    )
    .unwrap_err();

    assert!(error.to_string().contains("unsafe archive path"));
    assert!(!destination.path().join("escaped").exists());
}

/// A data bundle packaged from a snapshot built by the real snapshot builder.
struct ValidBundle {
    _source: TempDir,
    archive: PathBuf,
    snapshot_id: String,
}

impl ValidBundle {
    fn new() -> Self {
        let source = tempfile::tempdir().unwrap();
        let tree = source.path().join("tree");
        let corpus_dir = tree.join("corpus");
        fs::create_dir_all(&corpus_dir).unwrap();
        let zip_path = corpus_dir.join("intel.zip");
        write_zip(&zip_path, "intel.md", MARKDOWN.as_bytes());
        fs::write(
            corpus_dir.join("manifest.toml"),
            format!(
                "schema_version = 1\n\n[[archives]]\nid = \"intel\"\nvendor = \"intel\"\npath = \"intel.zip\"\nsha256 = \"{}\"\nentry_count = 1\nuncompressed_bytes = {}\n",
                sha256(&zip_path),
                MARKDOWN.len()
            ),
        )
        .unwrap();
        let config = AppConfig {
            root: tree.clone(),
            corpus_dir: corpus_dir.clone(),
            corpus_manifest: corpus_dir.join("manifest.toml"),
            index_dir: tree.join("index"),
            model_cache_dir: source.path().join("models"),
            snapshot_cache_dir: source.path().join("build-cache"),
        };
        let report = SnapshotBuilder::new(&config, &FakeEmbedder::new())
            .build(false)
            .unwrap();

        let archive = source.path().join("x86mcp-data.tar.zst");
        let encoder = zstd::Encoder::new(File::create(&archive).unwrap(), 3).unwrap();
        let mut tar = Builder::new(encoder);
        append(
            &mut tar,
            "x86mcp-data.json",
            bundle_metadata(env!("CARGO_PKG_VERSION"), &report.snapshot_id).as_bytes(),
        );
        tar.append_dir_all("corpus", &config.corpus_dir).unwrap();
        tar.append_dir_all("index", &config.index_dir).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        Self {
            _source: source,
            archive,
            snapshot_id: report.snapshot_id,
        }
    }

    fn options(&self, force: bool) -> SetupOptions {
        SetupOptions {
            data_source: Some(self.archive.display().to_string()),
            expected_sha256: Some(sha256(&self.archive)),
            force,
        }
    }
}

struct FakeEmbedder {
    spec: ModelSpec,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self {
            spec: FastEmbedder::production_spec().unwrap(),
        }
    }

    fn vector(&self, text: &str) -> Vec<f32> {
        let hash = blake3::hash(text.as_bytes());
        let mut vector = vec![0.0; self.spec.dimension];
        vector[0] = 1.0;
        vector[1] = f32::from(hash.as_bytes()[0]) / 255.0;
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
        Ok(texts.iter().map(|text| self.vector(text)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.vector(text))
    }
}

/// Keeps the snapshot cache inside the test's temporary data directory.
fn isolated_config(root: &Path) -> AppConfig {
    AppConfig {
        snapshot_cache_dir: root.join("snapshot-cache"),
        ..AppConfig::prepare_root(root).unwrap()
    }
}

fn open_installed(config: &AppConfig) -> Result<()> {
    Snapshot::open(&config.index_dir, &config.snapshot_cache_dir)?;
    Ok(())
}

fn bundle_metadata(version: &str, snapshot_id: &str) -> String {
    format!(
        "{{\"schema_version\":1,\"package_version\":\"{version}\",\"snapshot_id\":\"{snapshot_id}\"}}\n"
    )
}

fn write_path_traversal_bundle(path: &Path) {
    let encoder = zstd::Encoder::new(File::create(path).unwrap(), 3).unwrap();
    let mut archive = Builder::new(encoder);
    let mut header = Header::new_gnu();
    let entry_path = b"../../escaped";
    header.as_mut_bytes()[..entry_path.len()].copy_from_slice(entry_path);
    header.set_mode(0o644);
    header.set_size(3);
    header.set_cksum();
    archive.append(&header, Cursor::new(b"bad")).unwrap();
    archive.finish().unwrap();
    archive.into_inner().unwrap().finish().unwrap();
}

/// A bundle with the right layout and metadata whose snapshot cannot be opened.
fn write_shape_only_bundle(path: &Path, version: &str, snapshot_id: &str) {
    let encoder = zstd::Encoder::new(File::create(path).unwrap(), 3).unwrap();
    let mut archive = Builder::new(encoder);
    append(
        &mut archive,
        "x86mcp-data.json",
        bundle_metadata(version, snapshot_id).as_bytes(),
    );
    append(
        &mut archive,
        "corpus/manifest.toml",
        b"schema_version = 1\narchives = []\n",
    );
    append(&mut archive, "index/CURRENT", snapshot_id.as_bytes());
    append(
        &mut archive,
        &format!("index/snapshots/{snapshot_id}/snapshot.json"),
        format!("{{\"snapshot_id\":\"{snapshot_id}\",\"build_version\":\"{version}\"}}\n")
            .as_bytes(),
    );
    archive.finish().unwrap();
    archive.into_inner().unwrap().finish().unwrap();
}

fn append(archive: &mut Builder<zstd::Encoder<'_, File>>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn write_zip(path: &Path, entry: &str, bytes: &[u8]) {
    let mut writer = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file(entry, options).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap();
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
