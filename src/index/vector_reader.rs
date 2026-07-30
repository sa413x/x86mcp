use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    fs::File,
    io::Read,
    path::Path,
};

use memmap2::{Mmap, MmapOptions};
use rayon::prelude::*;

use super::{
    VectorError, VectorHit,
    vector_writer::{FORMAT_VERSION, HEADER_LEN, Header, MAGIC, normalize},
};

pub struct VectorReader {
    mapping: Mmap,
    dimension: usize,
    count: u64,
    payload_hash: String,
}

impl VectorReader {
    pub fn open(path: &Path, expected_dimension: usize) -> Result<Self, VectorError> {
        let mut file = File::open(path)?;
        let mut header_bytes = [0_u8; HEADER_LEN];
        file.read_exact(&mut header_bytes)?;
        let header = decode_header(&header_bytes)?;
        let actual_dimension = header.dimension as usize;
        if actual_dimension != expected_dimension {
            return Err(VectorError::DimensionMismatch {
                expected: expected_dimension,
                actual: actual_dimension,
            });
        }
        let computed_payload_len = header
            .count
            .checked_mul(u64::from(header.dimension))
            .and_then(|values| values.checked_mul(4))
            .ok_or(VectorError::SizeOverflow)?;
        if header.payload_len != computed_payload_len {
            return Err(VectorError::PayloadLengthMismatch {
                header: header.payload_len,
                computed: computed_payload_len,
            });
        }
        let expected_file_len = (HEADER_LEN as u64)
            .checked_add(header.payload_len)
            .ok_or(VectorError::SizeOverflow)?;
        let actual_file_len = file.metadata()?.len();
        if actual_file_len != expected_file_len {
            return Err(VectorError::FileLengthMismatch {
                expected: expected_file_len,
                actual: actual_file_len,
            });
        }

        // Safe: the header check above proves the mapping has the exact expected length.
        let mapping = unsafe { MmapOptions::new().map(&file)? };
        let payload = &mapping[HEADER_LEN..];
        if blake3::hash(payload).as_bytes() != &header.payload_hash {
            return Err(VectorError::PayloadHashMismatch);
        }
        let values = bytemuck::try_cast_slice::<u8, f32>(payload)
            .map_err(|_| VectorError::UnalignedPayload)?;
        validate_values(values, actual_dimension)?;
        let payload_hash = blake3::Hash::from_bytes(header.payload_hash)
            .to_hex()
            .to_string();
        Ok(Self {
            mapping,
            dimension: actual_dimension,
            count: header.count,
            payload_hash,
        })
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn payload_hash(&self) -> &str {
        &self.payload_hash
    }

    pub fn row(&self, row: u64) -> Result<&[f32], VectorError> {
        if row >= self.count {
            return Err(VectorError::InvalidAllowedRow {
                row,
                count: self.count,
            });
        }
        let row = usize::try_from(row).map_err(|_| VectorError::SizeOverflow)?;
        let start = row
            .checked_mul(self.dimension)
            .ok_or(VectorError::SizeOverflow)?;
        Ok(&self.values()[start..start + self.dimension])
    }

    pub fn top_k(
        &self,
        query: &[f32],
        allowed_rows: Option<&[u64]>,
        k: usize,
    ) -> Result<Vec<VectorHit>, VectorError> {
        if query.len() != self.dimension {
            return Err(VectorError::DimensionMismatch {
                expected: self.dimension,
                actual: query.len(),
            });
        }
        let count = usize::try_from(self.count).map_err(|_| VectorError::SizeOverflow)?;
        if k == 0 || k > count {
            return Err(VectorError::InvalidTopK);
        }
        if let Some(rows) = allowed_rows {
            validate_allowed_rows(rows, self.count)?;
        }
        let normalized_query = normalize(query, None)?;
        let vectors = self.values();
        let dimension = self.dimension;
        let heap = match allowed_rows {
            Some(rows) => rows
                .par_iter()
                .copied()
                .fold(
                    || BoundedHeap::new(k),
                    |mut heap, row| {
                        heap.push(Ranked {
                            row,
                            score: cosine(vectors, dimension, row, &normalized_query),
                        });
                        heap
                    },
                )
                .reduce(|| BoundedHeap::new(k), BoundedHeap::merge),
            None => (0..self.count)
                .into_par_iter()
                .fold(
                    || BoundedHeap::new(k),
                    |mut heap, row| {
                        heap.push(Ranked {
                            row,
                            score: cosine(vectors, dimension, row, &normalized_query),
                        });
                        heap
                    },
                )
                .reduce(|| BoundedHeap::new(k), BoundedHeap::merge),
        };
        Ok(heap.into_hits())
    }

    fn values(&self) -> &[f32] {
        bytemuck::cast_slice(&self.mapping[HEADER_LEN..])
    }
}

fn decode_header(bytes: &[u8; HEADER_LEN]) -> Result<Header, VectorError> {
    if bytes[..8] != MAGIC {
        return Err(VectorError::InvalidMagic);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed header range"));
    if version != FORMAT_VERSION {
        return Err(VectorError::UnsupportedVersion(version));
    }
    let dimension = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed header range"));
    let count = u64::from_le_bytes(bytes[16..24].try_into().expect("fixed header range"));
    let payload_len = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed header range"));
    let mut payload_hash = [0_u8; 32];
    payload_hash.copy_from_slice(&bytes[32..64]);
    Ok(Header {
        dimension,
        count,
        payload_len,
        payload_hash,
    })
}

fn validate_values(values: &[f32], dimension: usize) -> Result<(), VectorError> {
    for (row, vector) in values.chunks_exact(dimension).enumerate() {
        let row = u64::try_from(row).map_err(|_| VectorError::SizeOverflow)?;
        let mut squared_norm = 0.0_f64;
        for (column, &value) in vector.iter().enumerate() {
            if !value.is_finite() {
                return Err(VectorError::NonFiniteValue { row, column });
            }
            squared_norm += f64::from(value) * f64::from(value);
        }
        if (squared_norm.sqrt() - 1.0).abs() > 1e-4 {
            return Err(VectorError::NonUnitVector { row });
        }
    }
    Ok(())
}

fn validate_allowed_rows(rows: &[u64], count: u64) -> Result<(), VectorError> {
    let mut previous = None;
    for &row in rows {
        if row >= count {
            return Err(VectorError::InvalidAllowedRow { row, count });
        }
        if previous.is_some_and(|previous| previous >= row) {
            return Err(VectorError::UnsortedAllowedRows);
        }
        previous = Some(row);
    }
    Ok(())
}

fn cosine(vectors: &[f32], dimension: usize, row: u64, query: &[f32]) -> f32 {
    let start = row as usize * dimension;
    vectors[start..start + dimension]
        .iter()
        .zip(query)
        .map(|(&left, &right)| left * right)
        .sum()
}

#[derive(Clone, Copy, Debug)]
struct Ranked {
    row: u64,
    score: f32,
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row && self.score.to_bits() == other.score.to_bits()
    }
}

impl Eq for Ranked {}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.row.cmp(&self.row))
    }
}

struct BoundedHeap {
    limit: usize,
    entries: BinaryHeap<Reverse<Ranked>>,
}

impl BoundedHeap {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            entries: BinaryHeap::with_capacity(limit),
        }
    }

    fn push(&mut self, candidate: Ranked) {
        if self.entries.len() < self.limit {
            self.entries.push(Reverse(candidate));
        } else if self.entries.peek().is_some_and(|worst| candidate > worst.0) {
            self.entries.pop();
            self.entries.push(Reverse(candidate));
        }
    }

    fn merge(mut self, other: Self) -> Self {
        for Reverse(candidate) in other.entries {
            self.push(candidate);
        }
        self
    }

    fn into_hits(self) -> Vec<VectorHit> {
        let mut hits = self
            .entries
            .into_iter()
            .map(|Reverse(ranked)| VectorHit {
                row: ranked.row,
                score: ranked.score,
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.row.cmp(&right.row))
        });
        hits
    }
}
