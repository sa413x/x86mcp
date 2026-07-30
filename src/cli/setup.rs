use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::{
    config::AppConfig,
    index::{Embedder, FastEmbedder, model_artifacts_match},
    setup::{DataInstallReport, SetupOptions, install_data},
    snapshot::{Snapshot, SnapshotState, snapshot_status},
};

#[derive(Debug, Serialize)]
struct SetupReport {
    root: String,
    #[serde(flatten)]
    data: DataInstallReport,
    model_artifacts: usize,
    state: SnapshotState,
}

pub async fn run(config: &AppConfig, options: SetupOptions) -> Result<u8> {
    let config = config.clone();
    let report = tokio::task::spawn_blocking(move || setup(&config, &options))
        .await
        .context("setup worker failed")??;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(0)
}

fn setup(config: &AppConfig, options: &SetupOptions) -> Result<SetupReport> {
    let data = install_data(config, options)?;
    let snapshot = Snapshot::open(&config.index_dir, &config.snapshot_cache_dir)
        .context("opening installed snapshot")?;
    let embedder = FastEmbedder::new(config.model_cache_dir.clone())
        .context("downloading or opening the embedding model")?;
    ensure!(
        embedder.spec() == &snapshot.manifest.model,
        "installed snapshot expects a different embedding model contract"
    );
    let model_artifacts = embedder
        .artifact_hashes()
        .context("hashing embedding model artifacts")?;
    ensure!(
        model_artifacts_match(&model_artifacts, &snapshot.manifest.model_artifacts),
        "embedding model artifacts do not match the installed snapshot"
    );

    let status = snapshot_status(config);
    ensure!(
        status.state == SnapshotState::Ready,
        "installed snapshot is not ready: {}",
        status.reasons.join("; ")
    );
    Ok(SetupReport {
        root: config.root.display().to_string(),
        data,
        model_artifacts: model_artifacts.len(),
        state: status.state,
    })
}
