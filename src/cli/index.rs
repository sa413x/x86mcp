use anyhow::{Context, Result};

use crate::{config::AppConfig, index::FastEmbedder, snapshot::SnapshotBuilder};

pub async fn run(config: &AppConfig, force: bool) -> Result<u8> {
    let config = config.clone();
    let report = tokio::task::spawn_blocking(move || {
        let embedder = FastEmbedder::new(config.model_cache_dir.clone())?;
        SnapshotBuilder::new(&config, &embedder)
            .build(force)
            .map_err(anyhow::Error::from)
    })
    .await
    .context("index build task failed")??;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(0)
}
