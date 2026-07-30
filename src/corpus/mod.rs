mod archive;
mod entry;
mod manifest;
mod validation;

use std::{io, path::PathBuf, string::FromUtf8Error};

pub use manifest::{ArchiveSpec, CORPUS_SCHEMA_VERSION, CorpusManifest};
use thiserror::Error;

use crate::domain::document::ArchiveDocument;

#[derive(Debug, Error)]
pub enum CorpusError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot parse corpus manifest {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported corpus schema {found}; supported schema is {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("invalid corpus manifest: {reason}")]
    InvalidManifest { reason: String },
    #[error("archive {archive_id} checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        archive_id: String,
        expected: String,
        actual: String,
    },
    #[error("cannot read archive {archive_id}: {source}")]
    Zip {
        archive_id: String,
        #[source]
        source: zip::result::ZipError,
    },
    #[error("archive {archive_id} contains unsafe path {entry_path}")]
    UnsafePath {
        archive_id: String,
        entry_path: String,
    },
    #[error("archive {archive_id} contains duplicate normalized path {entry_path}")]
    DuplicatePath {
        archive_id: String,
        entry_path: String,
    },
    #[error("archive {archive_id} contains unsupported non-Markdown entry {entry_path}")]
    UnsupportedEntry {
        archive_id: String,
        entry_path: String,
    },
    #[error(
        "archive {archive_id} entry {entry_path} exceeds this platform's address space ({bytes} bytes)"
    )]
    EntryTooLarge {
        archive_id: String,
        entry_path: String,
        bytes: u64,
    },
    #[error("archive {archive_id} entry {entry_path} is not UTF-8: {source}")]
    InvalidUtf8 {
        archive_id: String,
        entry_path: String,
        #[source]
        source: FromUtf8Error,
    },
    #[error("archive {archive_id} {field} mismatch: expected {expected}, got {actual}")]
    MetadataMismatch {
        archive_id: String,
        field: &'static str,
        expected: u64,
        actual: u64,
    },
}

#[derive(Clone, Debug)]
pub struct CorpusReader {
    manifest: CorpusManifest,
    corpus_dir: PathBuf,
}

impl CorpusReader {
    pub fn new(manifest: CorpusManifest, corpus_dir: PathBuf) -> Self {
        Self {
            manifest,
            corpus_dir,
        }
    }

    pub fn read_all(&self) -> Result<Vec<ArchiveDocument>, CorpusError> {
        self.manifest.validate()?;
        let mut archives = self.manifest.archives.iter().collect::<Vec<_>>();
        archives.sort_unstable_by(|left, right| {
            (left.vendor, &left.id).cmp(&(right.vendor, &right.id))
        });

        let mut documents = Vec::new();
        for spec in archives {
            let relative = manifest::validate_relative_path(&spec.path).map_err(|reason| {
                CorpusError::InvalidManifest {
                    reason: format!("archive {} path: {reason}", spec.id),
                }
            })?;
            let archive_path = self.corpus_dir.join(relative);
            let actual = validation::sha256_file(&archive_path)?;
            if !actual.eq_ignore_ascii_case(&spec.sha256) {
                return Err(CorpusError::ChecksumMismatch {
                    archive_id: spec.id.clone(),
                    expected: spec.sha256.clone(),
                    actual,
                });
            }
            documents.extend(archive::read_archive(&archive_path, spec)?);
        }
        documents.sort_unstable_by(|left, right| {
            (
                left.meta.vendor,
                &left.meta.archive_id,
                &left.meta.entry_path,
            )
                .cmp(&(
                    right.meta.vendor,
                    &right.meta.archive_id,
                    &right.meta.entry_path,
                ))
        });
        Ok(documents)
    }
}
