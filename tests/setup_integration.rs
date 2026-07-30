use std::{fs, io::Cursor, path::Path};

use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use x86mcp::{
    config::AppConfig,
    setup::{SetupOptions, install_data},
};

const SNAPSHOT_ID: &str = "fixture-snapshot";

#[test]
fn installs_a_verified_data_bundle() {
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("x86mcp-data.tar.zst");
    write_bundle(&archive, env!("CARGO_PKG_VERSION"), SNAPSHOT_ID);
    let checksum = sha256(&archive);

    let destination = tempfile::tempdir().unwrap();
    let config = AppConfig::prepare_root(destination.path()).unwrap();
    let report = install_data(
        &config,
        &SetupOptions {
            data_source: Some(archive.display().to_string()),
            expected_sha256: Some(checksum),
            force: false,
        },
    )
    .unwrap();

    assert!(report.installed);
    assert_eq!(report.snapshot_id, SNAPSHOT_ID);
    assert_eq!(
        fs::read_to_string(destination.path().join("index/CURRENT")).unwrap(),
        SNAPSHOT_ID
    );
    assert!(destination.path().join("corpus/manifest.toml").is_file());
}

#[test]
fn checksum_failure_preserves_the_existing_installation() {
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("x86mcp-data.tar.zst");
    write_bundle(&archive, env!("CARGO_PKG_VERSION"), SNAPSHOT_ID);

    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("corpus")).unwrap();
    fs::write(destination.path().join("corpus/sentinel"), "keep").unwrap();
    let config = AppConfig::prepare_root(destination.path()).unwrap();
    let error = install_data(
        &config,
        &SetupOptions {
            data_source: Some(archive.display().to_string()),
            expected_sha256: Some("0".repeat(64)),
            force: true,
        },
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
    let config = AppConfig::prepare_root(destination.path()).unwrap();
    let error = install_data(
        &config,
        &SetupOptions {
            data_source: Some(archive.display().to_string()),
            expected_sha256: Some(sha256(&archive)),
            force: true,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("unsafe archive path"));
    assert!(!destination.path().join("escaped").exists());
}

fn write_path_traversal_bundle(path: &Path) {
    let encoder = zstd::Encoder::new(fs::File::create(path).unwrap(), 3).unwrap();
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

fn write_bundle(path: &Path, version: &str, snapshot_id: &str) {
    let encoder = zstd::Encoder::new(fs::File::create(path).unwrap(), 3).unwrap();
    let mut archive = Builder::new(encoder);
    append(
        &mut archive,
        "x86mcp-data.json",
        format!(
            "{{\"schema_version\":1,\"package_version\":\"{version}\",\"snapshot_id\":\"{snapshot_id}\"}}\n"
        )
        .as_bytes(),
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

fn append(archive: &mut Builder<zstd::Encoder<'_, fs::File>>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
