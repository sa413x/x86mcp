use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use x86mcp::{
    corpus::{CorpusError, CorpusManifest, CorpusReader},
    domain::document::ArchiveDocument,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub struct FixtureCorpus {
    _temp: TempDir,
    pub root: PathBuf,
    pub corpus_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub archive_path: PathBuf,
}

pub fn corpus_with_entry(vendor: &str, path: &str, bytes: &[u8]) -> FixtureCorpus {
    corpus_with_entries(vendor, &[(path, bytes)])
}

pub fn corpus_with_entries(vendor: &str, entries: &[(&str, &[u8])]) -> FixtureCorpus {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let corpus_dir = root.join("corpus");
    fs::create_dir(&corpus_dir).unwrap();
    let archive_name = format!("{vendor}.zip");
    let archive_path = corpus_dir.join(&archive_name);
    write_zip(&archive_path, entries);

    let sha256 = format!("{:x}", Sha256::digest(fs::read(&archive_path).unwrap()));
    let uncompressed_bytes = entries
        .iter()
        .map(|(_, bytes)| bytes.len() as u64)
        .sum::<u64>();
    let manifest_path = corpus_dir.join("manifest.toml");
    fs::write(
        &manifest_path,
        format!(
            "schema_version = 1\n\n[[archives]]\nid = \"{vendor}\"\nvendor = \"{vendor}\"\npath = \"{archive_name}\"\nsha256 = \"{sha256}\"\nentry_count = {}\nuncompressed_bytes = {uncompressed_bytes}\n",
            entries.len()
        ),
    )
    .unwrap();

    FixtureCorpus {
        _temp: temp,
        root,
        corpus_dir,
        manifest_path,
        archive_path,
    }
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn read_fixture(fixture: &FixtureCorpus) -> Result<Vec<ArchiveDocument>, CorpusError> {
    let manifest = CorpusManifest::load(&fixture.manifest_path)?;
    CorpusReader::new(manifest, fixture.corpus_dir.clone()).read_all()
}

#[test]
fn reads_utf8_markdown_without_extracting() {
    let fixture = corpus_with_entry("intel", "manual.md", b"# VMX\nEnable CR4.VMXE.");
    let docs = read_fixture(&fixture).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].meta.entry_path, "manual.md");
    assert_eq!(docs[0].source, "# VMX\nEnable CR4.VMXE.");
    assert!(!fixture.root.join("manual.md").exists());
}

#[test]
fn normalizes_safe_archive_dot_prefix() {
    let fixture = corpus_with_entry("intel", "./manual.md", b"# VMX");
    let docs = read_fixture(&fixture).unwrap();
    assert_eq!(docs[0].meta.entry_path, "manual.md");
}

#[test]
fn rejects_parent_traversal() {
    let fixture = corpus_with_entry("intel", "../manual.md", b"# VMX");
    let error = read_fixture(&fixture).unwrap_err();
    assert!(matches!(error, CorpusError::UnsafePath { .. }));
}

#[test]
fn rejects_manifest_checksum_mismatch() {
    let fixture = corpus_with_entry("intel", "manual.md", b"# VMX");
    OpenOptions::new()
        .append(true)
        .open(&fixture.archive_path)
        .unwrap()
        .write_all(b"tamper")
        .unwrap();
    let error = read_fixture(&fixture).unwrap_err();
    assert!(matches!(error, CorpusError::ChecksumMismatch { .. }));
}

#[test]
fn rejects_duplicate_normalized_paths() {
    let fixture = corpus_with_entries(
        "intel",
        &[
            ("dir\\manual.md", b"# First"),
            ("dir/manual.md", b"# Second"),
        ],
    );
    let error = read_fixture(&fixture).unwrap_err();
    assert!(matches!(error, CorpusError::DuplicatePath { .. }));
}
