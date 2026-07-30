use std::{env, path::PathBuf};

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
    pub fn from_root(root: impl Into<PathBuf>) -> Result<Self> {
        let root = std::fs::canonicalize(root.into()).context("canonicalizing project root")?;
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
        if current.join("corpus/manifest.toml").is_file() && current.join("index/CURRENT").is_file()
        {
            Ok(current)
        } else {
            Self::default_data_root()
        }
    }
}
