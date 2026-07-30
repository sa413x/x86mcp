use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;

use super::{
    SnapshotError,
    manifest::{CompressedArtifact, CompressedPart, blake3_file},
};

const ZSTD_LEVEL: i32 = 22;
const ZSTD_WINDOW_LOG: u32 = 29;

pub(crate) fn compress_artifact(
    raw_path: &Path,
    generation_path: &Path,
    base_name: &str,
    max_blob_bytes: u64,
    part_raw_bytes: u64,
) -> Result<CompressedArtifact, SnapshotError> {
    validate_filename(base_name)?;
    if max_blob_bytes == 0 || part_raw_bytes == 0 {
        return Err(SnapshotError::Invalid(
            "compressed artifact limits must be non-zero".into(),
        ));
    }

    let raw_bytes = fs::metadata(raw_path)
        .map_err(|source| SnapshotError::io(raw_path, source))?
        .len();
    let raw_blake3 = blake3_file(raw_path)?;
    let whole = compress_range(raw_path, generation_path, base_name, 0, raw_bytes)?;
    if whole.compressed_bytes <= max_blob_bytes {
        return Ok(CompressedArtifact {
            raw_bytes,
            raw_blake3,
            parts: vec![whole],
        });
    }

    let whole_path = generation_path.join(&whole.path);
    fs::remove_file(&whole_path).map_err(|source| SnapshotError::io(&whole_path, source))?;
    let stem = base_name.strip_suffix(".zst").unwrap_or(base_name);
    let mut parts: Vec<CompressedPart> = Vec::new();
    let mut offset = 0_u64;
    while offset < raw_bytes {
        let part_bytes = part_raw_bytes.min(raw_bytes - offset);
        let part_name = format!("{stem}.part{:03}.zst", parts.len() + 1);
        let part = compress_range(raw_path, generation_path, &part_name, offset, part_bytes)?;
        if part.compressed_bytes > max_blob_bytes {
            for created in &parts {
                let _ = fs::remove_file(generation_path.join(&created.path));
            }
            let part_path = generation_path.join(&part.path);
            let _ = fs::remove_file(&part_path);
            return Err(SnapshotError::Invalid(format!(
                "compressed artifact part {} is {} bytes, above limit {}",
                part.path, part.compressed_bytes, max_blob_bytes
            )));
        }
        parts.push(part);
        offset += part_bytes;
    }

    Ok(CompressedArtifact {
        raw_bytes,
        raw_blake3,
        parts,
    })
}

pub(crate) fn materialize_artifact(
    generation_path: &Path,
    cache_path: &Path,
    descriptor: &CompressedArtifact,
) -> Result<PathBuf, SnapshotError> {
    if descriptor.parts.is_empty() {
        return Err(SnapshotError::Invalid(
            "compressed artifact has no parts".into(),
        ));
    }
    let mut part_paths = Vec::with_capacity(descriptor.parts.len());
    for part in &descriptor.parts {
        validate_filename(&part.path)?;
        let part_path = generation_path.join(&part.path);
        let metadata =
            fs::metadata(&part_path).map_err(|source| SnapshotError::io(&part_path, source))?;
        if !metadata.is_file() || metadata.len() != part.compressed_bytes {
            return Err(SnapshotError::Invalid(format!(
                "compressed artifact {} size mismatch: expected {}, got {}",
                part.path,
                part.compressed_bytes,
                metadata.len()
            )));
        }
        let actual = blake3_file(&part_path)?;
        if actual != part.compressed_blake3 {
            return Err(SnapshotError::Invalid(format!(
                "compressed artifact {} checksum mismatch",
                part.path
            )));
        }
        part_paths.push(part_path);
    }

    if cache_is_valid(cache_path, descriptor)? {
        return Ok(cache_path.to_path_buf());
    }

    let parent = cache_path.parent().ok_or_else(|| {
        SnapshotError::Invalid(format!("cache path {} has no parent", cache_path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|source| SnapshotError::io(parent, source))?;
    let output = AtomicWriteFile::open(cache_path)
        .map_err(|source| SnapshotError::io(cache_path, source))?;
    let mut verified = IntegrityWriter::new(output);
    for part_path in &part_paths {
        let input = File::open(part_path).map_err(|source| SnapshotError::io(part_path, source))?;
        let mut decoder = zstd::stream::read::Decoder::new(input)
            .map_err(|source| SnapshotError::io(part_path, source))?;
        decoder
            .window_log_max(ZSTD_WINDOW_LOG)
            .map_err(|source| SnapshotError::io(part_path, source))?;
        io::copy(&mut decoder, &mut verified)
            .map_err(|source| SnapshotError::io(part_path, source))?;
    }
    verified
        .flush()
        .map_err(|source| SnapshotError::io(cache_path, source))?;
    let (output, raw_bytes, raw_blake3) = verified.finish();
    if raw_bytes != descriptor.raw_bytes || raw_blake3 != descriptor.raw_blake3 {
        output
            .discard()
            .map_err(|source| SnapshotError::io(cache_path, source))?;
        return Err(SnapshotError::Invalid(format!(
            "materialized artifact {} integrity mismatch",
            cache_path.display()
        )));
    }
    output
        .sync_all()
        .map_err(|source| SnapshotError::io(cache_path, source))?;
    if let Err(source) = output.commit() {
        if cache_is_valid(cache_path, descriptor)? {
            return Ok(cache_path.to_path_buf());
        }
        return Err(SnapshotError::io(cache_path, source));
    }
    Ok(cache_path.to_path_buf())
}

fn compress_range(
    raw_path: &Path,
    generation_path: &Path,
    name: &str,
    offset: u64,
    len: u64,
) -> Result<CompressedPart, SnapshotError> {
    validate_filename(name)?;
    let output_path = generation_path.join(name);
    let output =
        File::create(&output_path).map_err(|source| SnapshotError::io(&output_path, source))?;
    let mut encoder = zstd::stream::write::Encoder::new(output, ZSTD_LEVEL)
        .map_err(|source| SnapshotError::io(&output_path, source))?;
    encoder
        .include_checksum(true)
        .and_then(|()| encoder.long_distance_matching(true))
        .and_then(|()| encoder.window_log(ZSTD_WINDOW_LOG))
        .and_then(|()| encoder.include_contentsize(true))
        .and_then(|()| encoder.set_pledged_src_size(Some(len)))
        .map_err(|source| SnapshotError::io(&output_path, source))?;

    let mut input = File::open(raw_path).map_err(|source| SnapshotError::io(raw_path, source))?;
    input
        .seek(SeekFrom::Start(offset))
        .map_err(|source| SnapshotError::io(raw_path, source))?;
    let copied = io::copy(&mut input.take(len), &mut encoder)
        .map_err(|source| SnapshotError::io(raw_path, source))?;
    if copied != len {
        return Err(SnapshotError::Invalid(format!(
            "artifact {} changed while compressing: expected {len} bytes, read {copied}",
            raw_path.display()
        )));
    }
    let output = encoder
        .finish()
        .map_err(|source| SnapshotError::io(&output_path, source))?;
    output
        .sync_all()
        .map_err(|source| SnapshotError::io(&output_path, source))?;
    let compressed_bytes = output
        .metadata()
        .map_err(|source| SnapshotError::io(&output_path, source))?
        .len();
    Ok(CompressedPart {
        path: name.into(),
        compressed_bytes,
        compressed_blake3: blake3_file(&output_path)?,
    })
}

fn validate_filename(name: &str) -> Result<(), SnapshotError> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(SnapshotError::Invalid(format!(
            "unsafe compressed artifact path {name:?}"
        )));
    }
    Ok(())
}

fn cache_is_valid(
    cache_path: &Path,
    descriptor: &CompressedArtifact,
) -> Result<bool, SnapshotError> {
    let metadata = match fs::metadata(cache_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(SnapshotError::io(cache_path, source)),
    };
    if !metadata.is_file() || metadata.len() != descriptor.raw_bytes {
        return Ok(false);
    }
    Ok(blake3_file(cache_path)? == descriptor.raw_blake3)
}

struct IntegrityWriter<W> {
    inner: W,
    bytes: u64,
    hasher: blake3::Hasher,
}

impl<W> IntegrityWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            bytes: 0,
            hasher: blake3::Hasher::new(),
        }
    }

    fn finish(self) -> (W, u64, String) {
        (
            self.inner,
            self.bytes,
            self.hasher.finalize().to_hex().to_string(),
        )
    }
}

impl<W: Write> Write for IntegrityWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes = self
            .bytes
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("materialized artifact size overflow"))?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{compress_artifact, materialize_artifact};
    use crate::snapshot::manifest::{CompressedArtifact, CompressedPart};

    #[test]
    fn strong_zstd_round_trips_multipart_artifact() {
        let fixture = Fixture::new();
        let source = pseudorandom_bytes(256 * 1024);
        fs::write(&fixture.raw_path, &source).unwrap();

        let descriptor = compress_artifact(
            &fixture.raw_path,
            &fixture.generation,
            "catalog.sqlite3.zst",
            32 * 1024,
            16 * 1024,
        )
        .unwrap();

        assert!(descriptor.parts.len() > 1);
        assert!(
            descriptor
                .parts
                .iter()
                .all(|part| part.compressed_bytes <= 32 * 1024)
        );
        fs::remove_file(&fixture.raw_path).unwrap();
        let materialized =
            materialize_artifact(&fixture.generation, &fixture.cache_path, &descriptor).unwrap();
        assert_eq!(fs::read(materialized).unwrap(), source);
    }

    #[test]
    fn warm_cache_is_reused_without_rewriting() {
        let fixture = Fixture::new();
        let source = pseudorandom_bytes(64 * 1024);
        fs::write(&fixture.raw_path, &source).unwrap();
        let descriptor = compress_artifact(
            &fixture.raw_path,
            &fixture.generation,
            "vectors.f32.zst",
            128 * 1024,
            64 * 1024,
        )
        .unwrap();
        materialize_artifact(&fixture.generation, &fixture.cache_path, &descriptor).unwrap();

        let original_permissions = fs::metadata(&fixture.cache_path).unwrap().permissions();
        let mut readonly_permissions = original_permissions.clone();
        readonly_permissions.set_readonly(true);
        fs::set_permissions(&fixture.cache_path, readonly_permissions).unwrap();
        let result = materialize_artifact(&fixture.generation, &fixture.cache_path, &descriptor);
        fs::set_permissions(&fixture.cache_path, original_permissions).unwrap();

        assert_eq!(result.unwrap(), fixture.cache_path);
        assert_eq!(fs::read(&fixture.cache_path).unwrap(), source);
    }

    #[test]
    fn corrupt_frame_cannot_create_cache_artifact() {
        let fixture = Fixture::new();
        fs::write(&fixture.raw_path, pseudorandom_bytes(64 * 1024)).unwrap();
        let descriptor = compress_artifact(
            &fixture.raw_path,
            &fixture.generation,
            "vectors.f32.zst",
            128 * 1024,
            64 * 1024,
        )
        .unwrap();
        let part_path = fixture.generation.join(&descriptor.parts[0].path);
        let mut bytes = fs::read(&part_path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x80;
        fs::write(part_path, bytes).unwrap();

        assert!(
            materialize_artifact(&fixture.generation, &fixture.cache_path, &descriptor).is_err()
        );
        assert!(!fixture.cache_path.exists());
    }

    #[test]
    fn manifest_part_path_cannot_escape_generation() {
        let fixture = Fixture::new();
        let descriptor = CompressedArtifact {
            raw_bytes: 0,
            raw_blake3: blake3::hash(&[]).to_hex().to_string(),
            parts: vec![CompressedPart {
                path: "../outside.zst".into(),
                compressed_bytes: 0,
                compressed_blake3: blake3::hash(&[]).to_hex().to_string(),
            }],
        };

        assert!(
            materialize_artifact(&fixture.generation, &fixture.cache_path, &descriptor).is_err()
        );
        assert!(!fixture.cache_path.exists());
    }

    struct Fixture {
        _temporary: TempDir,
        generation: std::path::PathBuf,
        raw_path: std::path::PathBuf,
        cache_path: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let generation = temporary.path().join("generation");
            fs::create_dir_all(&generation).unwrap();
            Self {
                raw_path: generation.join("raw.bin"),
                cache_path: temporary.path().join("cache/raw.bin"),
                generation,
                _temporary: temporary,
            }
        }
    }

    fn pseudorandom_bytes(len: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }
}
