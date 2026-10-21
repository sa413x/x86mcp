use std::{
    env, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use directories::{BaseDirs, ProjectDirs};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub root: PathBuf,
    pub corpus_dir: PathBuf,
    pub corpus_manifest: PathBuf,
    pub index_dir: PathBuf,
    pub model_cache_dir: PathBuf,
    pub snapshot_cache_dir: PathBuf,
}

impl AppConfig {
    /// Resolves the configuration for `root` without creating anything. A root
    /// that does not exist yet (for example before `x86mcp setup`) is made
    /// absolute instead of canonicalized so callers can report it as missing.
    pub fn from_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = match std::fs::canonicalize(&root) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::path::absolute(&root).context("resolving project root")?
            }
            Err(error) => return Err(error).context("canonicalizing project root"),
        };
        let corpus_dir = root.join("corpus");
        let corpus_manifest = corpus_dir.join("manifest.toml");
        let index_dir = root.join("index");
        let project_cache_dir = ProjectDirs::from("dev", "x86mcp", "x86mcp")
            .context("resolving the user cache directory")?
            .cache_dir()
            .to_path_buf();
        let model_cache_dir = env::var_os("FASTEMBED_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| project_cache_dir.join("models"));
        let snapshot_cache_dir = env::var_os("X86MCP_SNAPSHOT_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| project_cache_dir.join("snapshots"));

        Ok(Self {
            root,
            corpus_dir,
            corpus_manifest,
            index_dir,
            model_cache_dir,
            snapshot_cache_dir,
        })
    }

    pub fn prepare_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating data directory {}", root.display()))?;
        Self::from_root(root)
    }

    pub fn default_data_root() -> Result<PathBuf> {
        Ok(BaseDirs::new()
            .context("resolving the user data directory")?
            .data_local_dir()
            .join("x86mcp"))
    }

    pub fn default_runtime_root() -> Result<PathBuf> {
        let current = env::current_dir().context("resolving the current directory")?;
        if is_source_checkout(&current) {
            Ok(current)
        } else {
            Self::default_data_root()
        }
    }
}

/// A source checkout is identified by its corpus manifest; its index may not
/// have been built yet, and `x86mcp index` must then build it in place.
fn is_source_checkout(dir: &Path) -> bool {
    dir.join("corpus/manifest.toml").is_file()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn source_checkout_without_published_snapshot_is_detected() {
        let checkout = tempfile::tempdir().unwrap();
        fs::create_dir_all(checkout.path().join("corpus")).unwrap();
        fs::create_dir_all(checkout.path().join("index")).unwrap();
        fs::write(
            checkout.path().join("corpus/manifest.toml"),
            "schema_version = 1\narchives = []\n",
        )
        .unwrap();

        assert!(is_source_checkout(checkout.path()));
    }

    #[test]
    fn directory_without_corpus_manifest_is_not_a_source_checkout() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("index")).unwrap();

        assert!(!is_source_checkout(directory.path()));
    }

    #[test]
    fn missing_root_resolves_without_creating_it() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("not-installed");

        let config = AppConfig::from_root(&root).unwrap();

        assert!(config.root.is_absolute());
        assert!(config.root.ends_with("not-installed"));
        assert!(!root.exists());
    }
}
