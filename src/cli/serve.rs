use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::{
    config::AppConfig,
    mcp,
    query::QueryEngine,
    snapshot::{Snapshot, StatusMonitor},
};

pub async fn run(config: &AppConfig) -> Result<u8> {
    let status = StatusMonitor::new(config);
    let startup = status.current_status();
    if !startup.is_usable() {
        bail!("no usable index snapshot: {}", startup.reasons.join("; "));
    }
    let snapshot = Snapshot::open(&config.index_dir, &config.snapshot_cache_dir)
        .context("opening current index snapshot")?;
    let engine = QueryEngine::production(snapshot, status, config.model_cache_dir.clone())
        .context("opening query engine")?;
    mcp::serve_stdio(Arc::new(engine)).await?;
    Ok(0)
}
