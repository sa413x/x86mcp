use std::{collections::HashSet, fs::File, io::Read, path::Path};

use zip::ZipArchive;

use super::{
    CorpusError, entry::normalize_markdown_path, manifest::ArchiveSpec, validation::sha256_bytes,
};
use crate::domain::document::{ArchiveDocument, DocumentMeta};

pub(crate) fn read_archive(
    archive_path: &Path,
    spec: &ArchiveSpec,
) -> Result<Vec<ArchiveDocument>, CorpusError> {
    let file = File::open(archive_path).map_err(|source| CorpusError::Io {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|source| CorpusError::Zip {
        archive_id: spec.id.clone(),
        source,
    })?;
    let mut documents = Vec::with_capacity(spec.entry_count as usize);
    let mut seen_paths = HashSet::with_capacity(spec.entry_count as usize);
    let mut observed_bytes = 0_u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|source| CorpusError::Zip {
            archive_id: spec.id.clone(),
            source,
        })?;
        if entry.is_dir() {
            continue;
        }

        let raw_path = entry.name().to_owned();
        let entry_path = normalize_markdown_path(&spec.id, &raw_path, &mut seen_paths)?;
        observed_bytes = observed_bytes.checked_add(entry.size()).ok_or_else(|| {
            CorpusError::MetadataMismatch {
                archive_id: spec.id.clone(),
                field: "uncompressed_bytes",
                expected: spec.uncompressed_bytes,
                actual: u64::MAX,
            }
        })?;
        if observed_bytes > spec.uncompressed_bytes {
            return Err(CorpusError::MetadataMismatch {
                archive_id: spec.id.clone(),
                field: "uncompressed_bytes",
                expected: spec.uncompressed_bytes,
                actual: observed_bytes,
            });
        }

        let capacity = usize::try_from(entry.size()).map_err(|_| CorpusError::EntryTooLarge {
            archive_id: spec.id.clone(),
            entry_path: entry_path.clone(),
            bytes: entry.size(),
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| CorpusError::Io {
                path: archive_path.to_path_buf(),
                source,
            })?;
        if bytes.len() as u64 != entry.size() {
            return Err(CorpusError::MetadataMismatch {
                archive_id: spec.id.clone(),
                field: "entry_size",
                expected: entry.size(),
                actual: bytes.len() as u64,
            });
        }
        let content_sha256 = sha256_bytes(&bytes);
        let source = String::from_utf8(bytes).map_err(|source| CorpusError::InvalidUtf8 {
            archive_id: spec.id.clone(),
            entry_path: entry_path.clone(),
            source,
        })?;
        let document_id = stable_document_id(&spec.id, &entry_path);
        documents.push(ArchiveDocument {
            meta: DocumentMeta {
                document_id,
                vendor: spec.vendor,
                archive_id: spec.id.clone(),
                archive_sha256: spec.sha256.clone(),
                entry_path,
                content_sha256,
                byte_len: source.len() as u64,
            },
            source,
        });
    }

    if documents.len() as u32 != spec.entry_count {
        return Err(CorpusError::MetadataMismatch {
            archive_id: spec.id.clone(),
            field: "entry_count",
            expected: spec.entry_count as u64,
            actual: documents.len() as u64,
        });
    }
    if observed_bytes != spec.uncompressed_bytes {
        return Err(CorpusError::MetadataMismatch {
            archive_id: spec.id.clone(),
            field: "uncompressed_bytes",
            expected: spec.uncompressed_bytes,
            actual: observed_bytes,
        });
    }

    documents.sort_unstable_by(|left, right| left.meta.entry_path.cmp(&right.meta.entry_path));
    Ok(documents)
}

fn stable_document_id(archive_id: &str, entry_path: &str) -> String {
    let material = format!("{archive_id}\0{entry_path}");
    format!("doc:{}", blake3::hash(material.as_bytes()).to_hex())
}
