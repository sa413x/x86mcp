use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokenizers::Tokenizer;

use super::vector_writer::normalize;

const MODEL: EmbeddingModel = EmbeddingModel::MultilingualE5Small;
const DIMENSION: usize = 384;
const MAX_TOKENS: usize = 512;
const QUERY_PREFIX: &str = "query: ";
const PASSAGE_PREFIX: &str = "passage: ";
const BATCH_SIZE: usize = 32;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ModelSpec {
    pub code: String,
    pub dimension: usize,
    pub max_tokens: usize,
    pub query_prefix: String,
    pub passage_prefix: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ModelArtifact {
    pub path: String,
    pub blake3: String,
}

pub trait Embedder: Send + Sync {
    fn spec(&self) -> &ModelSpec;
    fn count_tokens(&self, text: &str) -> Result<usize>;
    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    fn artifact_hashes(&self) -> Result<Vec<ModelArtifact>> {
        Ok(Vec::new())
    }
}

pub struct FastEmbedder {
    spec: ModelSpec,
    model: Mutex<TextEmbedding>,
    token_counter: Tokenizer,
    cache_dir: PathBuf,
}

impl FastEmbedder {
    pub fn production_spec() -> Result<ModelSpec> {
        let model_info = TextEmbedding::get_model_info(&MODEL)?;
        Ok(ModelSpec {
            code: model_info.model_code.clone(),
            dimension: DIMENSION,
            max_tokens: MAX_TOKENS,
            query_prefix: QUERY_PREFIX.into(),
            passage_prefix: PASSAGE_PREFIX.into(),
        })
    }

    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&cache_dir)
            .with_context(|| format!("failed to create model cache {}", cache_dir.display()))?;
        let model = TextEmbedding::try_new(
            TextInitOptions::new(MODEL)
                .with_cache_dir(cache_dir.clone())
                .with_max_length(MAX_TOKENS)
                .with_show_download_progress(false),
        )?;
        let mut token_counter = model.tokenizer.clone();
        token_counter
            .with_truncation(None)
            .map_err(anyhow::Error::msg)?;
        token_counter.with_padding(None);
        let spec = Self::production_spec()?;
        Ok(Self {
            spec,
            model: Mutex::new(model),
            token_counter,
            cache_dir,
        })
    }

    pub fn artifact_hashes(&self) -> Result<Vec<ModelArtifact>> {
        artifact_hashes_in(&self.cache_dir, &self.spec.code)
    }

    fn prefixed(&self, prefix: &str, text: &str) -> String {
        let mut output = String::with_capacity(prefix.len() + text.len());
        output.push_str(prefix);
        output.push_str(text);
        output
    }

    fn normalize_batch(&self, embeddings: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>> {
        embeddings
            .into_iter()
            .enumerate()
            .map(|(row, embedding)| {
                if embedding.len() != self.spec.dimension {
                    anyhow::bail!(
                        "embedding dimension mismatch: expected {}, got {}",
                        self.spec.dimension,
                        embedding.len()
                    );
                }
                normalize(&embedding, Some(row as u64)).map_err(anyhow::Error::from)
            })
            .collect()
    }
}

impl Embedder for FastEmbedder {
    fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    fn count_tokens(&self, text: &str) -> Result<usize> {
        let prefixed = self.prefixed(&self.spec.passage_prefix, text);
        let encoding = self
            .token_counter
            .encode(prefixed, true)
            .map_err(anyhow::Error::msg)?;
        Ok(encoding.len())
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prefixed = texts
            .iter()
            .map(|text| self.prefixed(&self.spec.passage_prefix, text))
            .collect::<Vec<_>>();
        let embeddings = self.model.lock().embed(prefixed, Some(BATCH_SIZE))?;
        if embeddings.len() != texts.len() {
            anyhow::bail!(
                "embedding count mismatch: expected {}, got {}",
                texts.len(),
                embeddings.len()
            );
        }
        self.normalize_batch(embeddings)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let prefixed = [self.prefixed(&self.spec.query_prefix, text)];
        let mut embeddings = self.model.lock().embed(prefixed, Some(1))?;
        if embeddings.len() != 1 {
            anyhow::bail!(
                "embedding model returned {} query vectors",
                embeddings.len()
            );
        }
        let embedding = embeddings.pop().expect("one query embedding");
        if embedding.len() != self.spec.dimension {
            anyhow::bail!(
                "embedding dimension mismatch: expected {}, got {}",
                self.spec.dimension,
                embedding.len()
            );
        }
        normalize(&embedding, None).map_err(anyhow::Error::from)
    }

    fn artifact_hashes(&self) -> Result<Vec<ModelArtifact>> {
        FastEmbedder::artifact_hashes(self)
    }
}

pub(crate) fn model_snapshot_artifacts(
    artifacts: &[ModelArtifact],
) -> impl Iterator<Item = &ModelArtifact> {
    artifacts
        .iter()
        .filter(|artifact| is_model_snapshot_path(&artifact.path))
}

pub(crate) fn model_artifacts_match(actual: &[ModelArtifact], expected: &[ModelArtifact]) -> bool {
    model_snapshot_artifacts(actual).eq(model_snapshot_artifacts(expected))
}

fn artifact_hashes_in(cache_dir: &Path, model_code: &str) -> Result<Vec<ModelArtifact>> {
    let repository_name = format!("models--{}", model_code.replace('/', "--"));
    let repository = cache_dir.join(repository_name);
    let revision = fs::read_to_string(repository.join("refs/main"))
        .context("reading active embedding model revision")?;
    let revision = revision.trim();
    ensure!(
        !revision.is_empty()
            && revision != "."
            && revision != ".."
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "embedding model cache has an invalid active revision"
    );

    let mut paths = Vec::new();
    collect_files(
        cache_dir,
        &repository.join("snapshots").join(revision),
        &mut paths,
    )?;
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    paths
        .into_iter()
        .map(|(relative, path)| {
            Ok(ModelArtifact {
                path: relative,
                blake3: hash_file(&path)?,
            })
        })
        .collect()
}

fn is_model_snapshot_path(path: &str) -> bool {
    let mut components = path.split('/');
    components
        .next()
        .is_some_and(|repository| repository.starts_with("models--"))
        && components.next() == Some("snapshots")
        && components
            .next()
            .is_some_and(|revision| !revision.is_empty())
        && components.next().is_some()
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            collect_files(root, &path, output)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .context("model artifact escaped cache root")?
                .to_string_lossy()
                .replace('\\', "/");
            output.push((relative, path));
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        DIMENSION, Embedder, FastEmbedder, ModelArtifact, artifact_hashes_in, model_artifacts_match,
    };

    #[test]
    fn artifact_identity_ignores_hugging_face_cache_plumbing() {
        let cache = tempfile::tempdir().unwrap();
        let repository = cache.path().join("models--vendor--model");
        fs::create_dir_all(repository.join("blobs")).unwrap();
        fs::create_dir_all(repository.join("refs")).unwrap();
        fs::create_dir_all(repository.join("snapshots/revision/onnx")).unwrap();
        fs::write(repository.join("blobs/content"), b"model").unwrap();
        fs::write(repository.join("blobs/content.lock"), b"").unwrap();
        fs::write(repository.join("refs/main"), b"revision").unwrap();
        fs::write(repository.join("snapshots/revision/config.json"), b"config").unwrap();
        fs::write(
            repository.join("snapshots/revision/onnx/model.onnx"),
            b"model",
        )
        .unwrap();
        fs::create_dir_all(repository.join("snapshots/old")).unwrap();
        fs::write(repository.join("snapshots/old/model.onnx"), b"old").unwrap();
        let unrelated = cache
            .path()
            .join("models--vendor--other/snapshots/revision");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("model.onnx"), b"other").unwrap();

        let artifacts = artifact_hashes_in(cache.path(), "vendor/model").unwrap();

        assert_eq!(
            artifacts
                .iter()
                .map(|artifact| artifact.path.as_str())
                .collect::<Vec<_>>(),
            [
                "models--vendor--model/snapshots/revision/config.json",
                "models--vendor--model/snapshots/revision/onnx/model.onnx",
            ]
        );
    }

    #[test]
    fn legacy_cache_metadata_matches_snapshot_files() {
        let snapshot = ModelArtifact {
            path: "models--vendor--model/snapshots/revision/model.onnx".into(),
            blake3: "model-hash".into(),
        };
        let legacy = vec![
            ModelArtifact {
                path: "models--vendor--model/blobs/content.lock".into(),
                blake3: "empty-hash".into(),
            },
            snapshot.clone(),
            ModelArtifact {
                path: "models--vendor--model/refs/main".into(),
                blake3: "revision-hash".into(),
            },
        ];

        assert!(model_artifacts_match(
            std::slice::from_ref(&snapshot),
            &legacy
        ));
    }

    #[test]
    #[ignore = "downloads the pinned production embedding artifacts"]
    fn fastembed_model_smoke() {
        let cache = std::env::temp_dir().join("x86mcp-fastembed-model-cache");
        let embedder = FastEmbedder::new(cache).unwrap();
        for query in [
            "How does CR4.VMXE enable VMX?",
            "Как бит CR4.VMXE включает виртуализацию?",
        ] {
            let vector = embedder.embed_query(query).unwrap();
            assert_eq!(vector.len(), DIMENSION);
            assert!(vector.iter().all(|value| value.is_finite()));
            let norm = vector
                .iter()
                .map(|value| f64::from(*value) * f64::from(*value))
                .sum::<f64>()
                .sqrt();
            assert!((norm - 1.0).abs() < 1e-5);
        }
        assert!(embedder.count_tokens(&"VMX ".repeat(600)).unwrap() > 512);
        assert!(!embedder.artifact_hashes().unwrap().is_empty());
    }
}
