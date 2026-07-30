use anyhow::{Context, Result};

use crate::{
    config::AppConfig,
    snapshot::{SnapshotState, snapshot_status},
};

pub async fn run(config: &AppConfig) -> Result<u8> {
    let config = config.clone();
    let status = tokio::task::spawn_blocking(move || snapshot_status(&config))
        .await
        .context("snapshot status task failed")?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(match status.state {
        SnapshotState::Ready | SnapshotState::DegradedModel => 0,
        SnapshotState::Stale | SnapshotState::Invalid => 1,
        SnapshotState::Missing => 2,
    })
}
