use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use sha2::{Digest, Sha256};

use super::CorpusError;

pub(crate) fn sha256_file(path: &Path) -> Result<String, CorpusError> {
    let file = File::open(path).map_err(|source| CorpusError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|source| CorpusError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
