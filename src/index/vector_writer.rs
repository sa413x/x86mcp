use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use atomic_write_file::AtomicWriteFile;

use super::{VectorBuildStats, VectorError};

pub(crate) const MAGIC: [u8; 8] = *b"X86VEC1\0";
pub(crate) const FORMAT_VERSION: u32 = 1;
pub(crate) const HEADER_LEN: usize = 64;

pub struct VectorWriter;

impl VectorWriter {
    pub fn write(
        path: &Path,
        dimension: usize,
        vectors: &[Vec<f32>],
    ) -> Result<VectorBuildStats, VectorError> {
        let dimension_u32 = u32::try_from(dimension).map_err(|_| VectorError::SizeOverflow)?;
        if dimension == 0 {
            return Err(VectorError::DimensionMismatch {
                expected: 1,
                actual: 0,
            });
        }
        let count = u64::try_from(vectors.len()).map_err(|_| VectorError::SizeOverflow)?;
        let payload_len = count
            .checked_mul(u64::from(dimension_u32))
            .and_then(|values| values.checked_mul(4))
            .ok_or(VectorError::SizeOverflow)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = AtomicWriteFile::open(path)?;
        file.write_all(&[0_u8; HEADER_LEN])?;
        let mut hasher = blake3::Hasher::new();
        for (row, vector) in vectors.iter().enumerate() {
            if vector.len() != dimension {
                return Err(VectorError::DimensionMismatch {
                    expected: dimension,
                    actual: vector.len(),
                });
            }
            let row = u64::try_from(row).map_err(|_| VectorError::SizeOverflow)?;
            let normalized = normalize(vector, Some(row))?;
            for value in normalized {
                let bytes = value.to_le_bytes();
                file.write_all(&bytes)?;
                hasher.update(&bytes);
            }
        }
        let payload_hash = hasher.finalize();
        let header = Header {
            dimension: dimension_u32,
            count,
            payload_len,
            payload_hash: *payload_hash.as_bytes(),
        };
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.encode())?;
        file.commit()?;

        Ok(VectorBuildStats {
            count,
            dimension,
            payload_hash: payload_hash.to_hex().to_string(),
        })
    }
}

pub(crate) struct Header {
    pub dimension: u32,
    pub count: u64,
    pub payload_len: u64,
    pub payload_hash: [u8; 32],
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut bytes = [0_u8; HEADER_LEN];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dimension.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes[32..64].copy_from_slice(&self.payload_hash);
        bytes
    }
}

pub(crate) fn normalize(values: &[f32], row: Option<u64>) -> Result<Vec<f32>, VectorError> {
    let mut squared_norm = 0.0_f64;
    for (column, &value) in values.iter().enumerate() {
        if !value.is_finite() {
            return Err(VectorError::NonFiniteValue {
                row: row.unwrap_or(0),
                column,
            });
        }
        squared_norm += f64::from(value) * f64::from(value);
    }
    if squared_norm == 0.0 {
        return Err(VectorError::ZeroNorm { row });
    }
    let norm = squared_norm.sqrt();
    values
        .iter()
        .enumerate()
        .map(|(column, &value)| {
            let normalized = (f64::from(value) / norm) as f32;
            if normalized.is_finite() {
                Ok(normalized)
            } else {
                Err(VectorError::NonFiniteValue {
                    row: row.unwrap_or(0),
                    column,
                })
            }
        })
        .collect()
}
