use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::AppConfig,
    snapshot::{IndexLock, Snapshot},
};

const BUNDLE_SCHEMA_VERSION: u32 = 1;
const BUNDLE_METADATA: &str = "x86mcp-data.json";
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_CHECKSUM_BYTES: u64 = 1_024;
const RELEASE_BASE_URL: &str = "https://github.com/sa413x/x86mcp/releases/download";
const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default)]
pub struct SetupOptions {
    pub data_source: Option<String>,
    pub expected_sha256: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DataBundleManifest {
    pub schema_version: u32,
    pub package_version: String,
    pub snapshot_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DataInstallReport {
    pub installed: bool,
    pub package_version: String,
    pub snapshot_id: String,
    pub data_source: String,
    pub archive_sha256: Option<String>,
    pub bytes_downloaded: u64,
}

pub fn default_data_url() -> String {
    release_data_url(option_env!("X86MCP_RELEASE_TAG"))
}

/// The release workflow sets `X86MCP_RELEASE_TAG` to the exact Git tag it
/// publishes; local builds fall back to the conventional `v<version>` tag.
fn release_data_url(release_tag: Option<&str>) -> String {
    let tag = match release_tag.map(str::trim) {
        Some(tag) if !tag.is_empty() => tag.to_owned(),
        _ => format!("v{PACKAGE_VERSION}"),
    };
    format!("{RELEASE_BASE_URL}/{tag}/x86mcp-data-{PACKAGE_VERSION}.tar.zst")
}

/// Installs the data bundle and runs `verify` against the installed data.
///
/// A replaced installation is kept as a backup until `verify` succeeds; if
/// publishing or `verify` fails, the previous installation is restored.
/// `verify` must close every file it opens under the data root before
/// returning so a rollback can move the new files away.
pub fn install_data<T>(
    config: &AppConfig,
    options: &SetupOptions,
    verify: impl FnOnce() -> Result<T>,
) -> Result<(DataInstallReport, T)> {
    fs::create_dir_all(&config.root)
        .with_context(|| format!("creating data directory {}", config.root.display()))?;
    let lock_path = config.root.join(".x86mcp-setup.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening setup lock {}", lock_path.display()))?;
    lock.try_lock_exclusive()
        .context("another x86mcp setup is already running for this data directory")?;

    let result = install_data_locked(config, options, verify);
    let _ = FileExt::unlock(&lock);
    result
}

fn install_data_locked<T>(
    config: &AppConfig,
    options: &SetupOptions,
    verify: impl FnOnce() -> Result<T>,
) -> Result<(DataInstallReport, T)> {
    if !options.force
        && let Ok(manifest) = load_bundle_manifest(&config.root.join(BUNDLE_METADATA))
        && manifest.package_version == PACKAGE_VERSION
        && validate_installed_shape(&config.root, &manifest).is_ok()
    {
        let verified = verify().context("verifying the existing data installation")?;
        let report = DataInstallReport {
            installed: false,
            package_version: manifest.package_version,
            snapshot_id: manifest.snapshot_id,
            data_source: "existing installation".into(),
            archive_sha256: None,
            bytes_downloaded: 0,
        };
        return Ok((report, verified));
    }

    let source = options.data_source.clone().unwrap_or_else(default_data_url);
    let staging = tempfile::Builder::new()
        .prefix(".x86mcp-setup-")
        .tempdir_in(&config.root)
        .with_context(|| format!("creating setup staging area in {}", config.root.display()))?;
    let archive_path = staging.path().join("data.tar.zst");
    let (actual_sha256, bytes_downloaded) = fetch_to_file(&source, &archive_path)?;
    let expected_sha256 = match &options.expected_sha256 {
        Some(value) => parse_sha256(value)?,
        None => fetch_expected_sha256(&source)?,
    };
    ensure!(
        actual_sha256 == expected_sha256,
        "data archive checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
    );

    let payload = staging.path().join("payload");
    fs::create_dir(&payload).context("creating extracted data directory")?;
    extract_archive(&archive_path, &payload)?;
    let manifest = validate_bundle(&payload)?;
    validate_staged_snapshot(&payload, staging.path())?;

    // Index builds must not run while their directory is swapped out.
    let _index_lock = IndexLock::acquire(&config.index_dir)
        .context("waiting for running index builds to finish")?;
    let backup = staging.path().join("backup");
    fs::create_dir(&backup).context("creating setup rollback directory")?;
    let mut publication = Publication::default();
    let outcome = publish_payload(&config.root, &payload, &backup, &mut publication)
        .and_then(|()| verify().context("verifying the installed data bundle"));
    let verified = match outcome {
        Ok(verified) => verified,
        Err(error) => {
            if let Err(rollback_error) = rollback_publish(&config.root, &backup, &publication) {
                let kept = staging.keep().join("backup");
                return Err(error.context(format!(
                    "restoring the previous installation failed ({rollback_error}); its files are kept in {}",
                    kept.display()
                )));
            }
            return Err(error);
        }
    };

    let report = DataInstallReport {
        installed: true,
        package_version: manifest.package_version,
        snapshot_id: manifest.snapshot_id,
        data_source: source,
        archive_sha256: Some(actual_sha256),
        bytes_downloaded,
    };
    Ok((report, verified))
}

/// Opens the staged snapshot exactly as `serve` would, with a throwaway
/// snapshot cache, so a broken bundle never replaces a working installation.
fn validate_staged_snapshot(payload: &Path, staging: &Path) -> Result<()> {
    let snapshot = Snapshot::open(&payload.join("index"), &staging.join("snapshot-cache"))
        .context("validating the data bundle snapshot")?;
    // Close every staged file before the payload directories are moved.
    drop(snapshot);
    Ok(())
}

fn fetch_to_file(source: &str, destination: &Path) -> Result<(String, u64)> {
    let file = File::create(destination)
        .with_context(|| format!("creating download file {}", destination.display()))?;
    if source.starts_with("http://") {
        bail!("insecure data URL is not allowed; use HTTPS");
    }

    if source.starts_with("https://") {
        let response = ureq::get(source)
            .header("User-Agent", concat!("x86mcp/", env!("CARGO_PKG_VERSION")))
            .call()
            .with_context(|| format!("downloading data archive from {source}"))?;
        if let Some(length) = response.body().content_length() {
            ensure!(
                length <= MAX_ARCHIVE_BYTES,
                "data archive is too large: {length} bytes"
            );
        }
        copy_and_hash(response.into_body().into_reader(), file)
    } else {
        let source_path = Path::new(source);
        let metadata = fs::metadata(source_path)
            .with_context(|| format!("reading data archive metadata {}", source_path.display()))?;
        ensure!(
            metadata.len() <= MAX_ARCHIVE_BYTES,
            "data archive is too large: {} bytes",
            metadata.len()
        );
        let input = File::open(source_path)
            .with_context(|| format!("opening data archive {}", source_path.display()))?;
        copy_and_hash(input, file)
    }
}

fn copy_and_hash(mut input: impl Read, mut output: File) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = input.read(&mut buffer).context("reading data archive")?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .context("data archive size overflow")?;
        ensure!(
            total <= MAX_ARCHIVE_BYTES,
            "data archive exceeds the {} byte limit",
            MAX_ARCHIVE_BYTES
        );
        output
            .write_all(&buffer[..read])
            .context("writing downloaded data archive")?;
        hasher.update(&buffer[..read]);
    }
    output
        .sync_all()
        .context("syncing downloaded data archive")?;
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn fetch_expected_sha256(source: &str) -> Result<String> {
    let checksum_source = format!("{source}.sha256");
    let text = if checksum_source.starts_with("https://") {
        let mut response = ureq::get(&checksum_source)
            .header("User-Agent", concat!("x86mcp/", env!("CARGO_PKG_VERSION")))
            .call()
            .with_context(|| format!("downloading checksum from {checksum_source}"))?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_CHECKSUM_BYTES)
            .read_to_string()
            .context("reading checksum response")?
    } else {
        let path = Path::new(&checksum_source);
        let metadata = fs::metadata(path)
            .with_context(|| format!("reading checksum metadata {}", path.display()))?;
        ensure!(
            metadata.len() <= MAX_CHECKSUM_BYTES,
            "checksum file is too large"
        );
        fs::read_to_string(path)
            .with_context(|| format!("reading checksum file {}", path.display()))?
    };
    parse_sha256(&text)
}

fn parse_sha256(value: &str) -> Result<String> {
    let checksum = value
        .split_whitespace()
        .next()
        .context("checksum is empty")?;
    ensure!(
        checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "checksum must be exactly 64 hexadecimal characters"
    );
    Ok(checksum.to_ascii_lowercase())
}

fn extract_archive(archive_path: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("opening data archive {}", archive_path.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file).context("opening zstd data stream")?;
    let mut archive = tar::Archive::new(decoder);
    let mut paths = HashSet::new();
    let mut total_size = 0_u64;
    let mut entry_count = 0_usize;

    for entry in archive.entries().context("reading tar data archive")? {
        let mut entry = entry.context("reading tar entry")?;
        entry_count += 1;
        ensure!(
            entry_count <= MAX_ARCHIVE_ENTRIES,
            "data archive contains too many entries"
        );
        let path = entry.path().context("reading tar entry path")?.into_owned();
        validate_archive_path(&path)?;
        ensure!(
            paths.insert(path.clone()),
            "duplicate archive path: {}",
            path.display()
        );

        let entry_type = entry.header().entry_type();
        ensure!(
            entry_type.is_file() || entry_type.is_dir(),
            "unsupported archive entry type for {}",
            path.display()
        );
        if entry_type.is_dir() {
            fs::create_dir_all(destination.join(&path))
                .with_context(|| format!("creating extracted directory {}", path.display()))?;
            continue;
        }

        ensure!(
            path != Path::new("corpus") && path != Path::new("index"),
            "archive root {} must be a directory",
            path.display()
        );
        let size = entry.size();
        total_size = total_size
            .checked_add(size)
            .context("expanded data size overflow")?;
        ensure!(
            total_size <= MAX_EXPANDED_BYTES,
            "expanded data exceeds the {} byte limit",
            MAX_EXPANDED_BYTES
        );
        let output_path = destination.join(&path);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating extracted directory {}", parent.display()))?;
        }
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output_path)
            .with_context(|| format!("creating extracted file {}", output_path.display()))?;
        let copied = std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("extracting {}", path.display()))?;
        ensure!(copied == size, "short tar entry for {}", path.display());
    }
    Ok(())
}

fn validate_archive_path(path: &Path) -> Result<()> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(value)) => value,
        _ => bail!("unsafe archive path: {}", path.display()),
    };
    ensure!(
        first == "corpus" || first == "index" || first == BUNDLE_METADATA,
        "unexpected archive root: {}",
        path.display()
    );
    if first == BUNDLE_METADATA {
        ensure!(
            components.next().is_none(),
            "bundle metadata must be at the archive root"
        );
    } else {
        for component in components {
            ensure!(
                matches!(component, Component::Normal(_)),
                "unsafe archive path: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn validate_bundle(root: &Path) -> Result<DataBundleManifest> {
    let manifest = load_bundle_manifest(&root.join(BUNDLE_METADATA))?;
    ensure!(
        manifest.schema_version == BUNDLE_SCHEMA_VERSION,
        "unsupported data bundle schema version {}",
        manifest.schema_version
    );
    ensure!(
        manifest.package_version == PACKAGE_VERSION,
        "data bundle version {} does not match x86mcp {PACKAGE_VERSION}",
        manifest.package_version
    );
    validate_installed_shape(root, &manifest)?;
    Ok(manifest)
}

fn load_bundle_manifest(path: &Path) -> Result<DataBundleManifest> {
    let file = File::open(path)
        .with_context(|| format!("opening data bundle metadata {}", path.display()))?;
    serde_json::from_reader(file)
        .with_context(|| format!("parsing data bundle metadata {}", path.display()))
}

fn validate_installed_shape(root: &Path, manifest: &DataBundleManifest) -> Result<()> {
    ensure!(!manifest.snapshot_id.is_empty(), "snapshot ID is empty");
    ensure!(
        manifest.snapshot_id.len() <= 128
            && manifest
                .snapshot_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "snapshot ID contains unsafe characters"
    );
    ensure!(
        root.join("corpus/manifest.toml").is_file(),
        "data bundle is missing corpus/manifest.toml"
    );
    let current_path = root.join("index/CURRENT");
    let current = fs::read_to_string(&current_path)
        .with_context(|| format!("reading {}", current_path.display()))?;
    ensure!(
        current.trim() == manifest.snapshot_id,
        "index/CURRENT does not match the data bundle snapshot"
    );
    let snapshot_manifest_path = root
        .join("index")
        .join("snapshots")
        .join(&manifest.snapshot_id)
        .join("snapshot.json");
    let snapshot_manifest: serde_json::Value = serde_json::from_reader(
        File::open(&snapshot_manifest_path)
            .with_context(|| format!("opening {}", snapshot_manifest_path.display()))?,
    )
    .with_context(|| format!("parsing {}", snapshot_manifest_path.display()))?;
    ensure!(
        snapshot_manifest["snapshot_id"].as_str() == Some(manifest.snapshot_id.as_str()),
        "snapshot manifest ID does not match the data bundle"
    );
    ensure!(
        snapshot_manifest["build_version"].as_str() == Some(PACKAGE_VERSION),
        "snapshot build version does not match x86mcp {PACKAGE_VERSION}"
    );
    Ok(())
}

/// Paths moved during publication, in the order they were moved.
#[derive(Default)]
struct Publication {
    backed_up: Vec<&'static str>,
    installed: Vec<&'static str>,
}

/// Moves the payload into `root`, moving any existing installation into
/// `backup`. The backup is left in place for the caller to discard or restore.
fn publish_payload(
    root: &Path,
    payload: &Path,
    backup: &Path,
    publication: &mut Publication,
) -> Result<()> {
    for name in ["corpus", "index", BUNDLE_METADATA] {
        let target = root.join(name);
        if target.exists() {
            fs::rename(&target, backup.join(name))
                .with_context(|| format!("moving existing {} aside", target.display()))?;
            publication.backed_up.push(name);
        }
        fs::rename(payload.join(name), &target)
            .with_context(|| format!("publishing {}", target.display()))?;
        publication.installed.push(name);
    }
    Ok(())
}

fn rollback_publish(root: &Path, backup: &Path, publication: &Publication) -> std::io::Result<()> {
    let mut first_error = None;
    for name in publication.installed.iter().rev() {
        if let Err(error) = remove_path(&root.join(name)) {
            first_error.get_or_insert(error);
        }
    }
    for name in publication.backed_up.iter().rev() {
        if let Err(error) = fs::rename(backup.join(name), root.join(name)) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_data_url_preserves_the_exact_release_tag() {
        for tag in ["0.1.0", "v0.1.0", "x86mcp/0.1.0", "x86mcp-v0.1.0"] {
            assert_eq!(
                release_data_url(Some(tag)),
                format!("{RELEASE_BASE_URL}/{tag}/x86mcp-data-{PACKAGE_VERSION}.tar.zst")
            );
        }
    }

    #[test]
    fn release_data_url_falls_back_to_the_version_tag() {
        let expected =
            format!("{RELEASE_BASE_URL}/v{PACKAGE_VERSION}/x86mcp-data-{PACKAGE_VERSION}.tar.zst");
        assert_eq!(release_data_url(None), expected);
        assert_eq!(release_data_url(Some("")), expected);
    }
}
