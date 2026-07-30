use std::{collections::HashSet, fs, path::Path};

use serde::{Deserialize, Serialize};

use super::CorpusError;
use crate::domain::Vendor;

pub const CORPUS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CorpusManifest {
    pub schema_version: u32,
    pub archives: Vec<ArchiveSpec>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArchiveSpec {
    pub id: String,
    pub vendor: Vendor,
    pub path: String,
    pub sha256: String,
    pub entry_count: u32,
    pub uncompressed_bytes: u64,
}

impl CorpusManifest {
    pub fn load(path: &Path) -> Result<Self, CorpusError> {
        let source = fs::read_to_string(path).map_err(|source| CorpusError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest: Self =
            toml::from_str(&source).map_err(|source| CorpusError::ManifestParse {
                path: path.to_path_buf(),
                source,
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.schema_version != CORPUS_SCHEMA_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                found: self.schema_version,
                supported: CORPUS_SCHEMA_VERSION,
            });
        }
        if self.archives.is_empty() {
            return Err(CorpusError::InvalidManifest {
                reason: "at least one archive is required".into(),
            });
        }

        let mut ids = HashSet::with_capacity(self.archives.len());
        for archive in &self.archives {
            if archive.id.trim().is_empty() {
                return Err(CorpusError::InvalidManifest {
                    reason: "archive id cannot be empty".into(),
                });
            }
            if !ids.insert(&archive.id) {
                return Err(CorpusError::InvalidManifest {
                    reason: format!("duplicate archive id {}", archive.id),
                });
            }
            if archive.sha256.len() != 64
                || !archive.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(CorpusError::InvalidManifest {
                    reason: format!("archive {} has an invalid SHA-256", archive.id),
                });
            }
            validate_relative_path(&archive.path).map_err(|reason| {
                CorpusError::InvalidManifest {
                    reason: format!("archive {} path: {reason}", archive.id),
                }
            })?;
        }
        Ok(())
    }
}

pub(crate) fn validate_relative_path(path: &str) -> Result<String, &'static str> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains('\0') {
        return Err("path must be a non-empty relative path");
    }
    let mut components = normalized.split('/');
    let first = components.next().ok_or("path must not be empty")?;
    if first.ends_with(':') || first.is_empty() || first == "." || first == ".." {
        return Err("path contains an unsafe component");
    }
    if components.any(|component| component.is_empty() || component == "." || component == "..") {
        return Err("path contains an unsafe component");
    }
    Ok(normalized)
}
