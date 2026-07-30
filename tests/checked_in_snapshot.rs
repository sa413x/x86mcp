use std::{fs, path::Path};

use anyhow::Result;
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::json;
use x86mcp::snapshot::{SNAPSHOT_SCHEMA_VERSION, SnapshotManifest};

#[tokio::test]
async fn checked_in_snapshot_serves_lexical_mcp_without_model_artifacts() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let snapshot_id = fs::read_to_string(root.join("index/CURRENT"))?
        .trim()
        .to_owned();
    let generation = root.join("index/snapshots").join(&snapshot_id);
    let manifest = SnapshotManifest::load(&generation.join("snapshot.json"))?;
    assert_eq!(manifest.schema_version, SNAPSHOT_SCHEMA_VERSION);
    assert_eq!(manifest.schema_version, 2);
    assert!(!generation.join("catalog.sqlite3").exists());
    assert!(!generation.join("vectors.f32").exists());
    for artifact in [&manifest.components.catalog, &manifest.components.vectors] {
        assert!(!artifact.parts.is_empty());
        for part in &artifact.parts {
            assert!(part.path.ends_with(".zst"));
            assert!(part.compressed_bytes < 100 * 1024 * 1024);
            assert_eq!(
                fs::metadata(generation.join(&part.path))?.len(),
                part.compressed_bytes
            );
        }
    }
    let snapshot_cache = tempfile::tempdir()?;
    let empty_model_cache = tempfile::tempdir()?;
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_x86mcp")).configure(|command| {
            command
                .arg("--root")
                .arg(env!("CARGO_MANIFEST_DIR"))
                .arg("serve")
                .env("FASTEMBED_CACHE_DIR", empty_model_cache.path())
                .env("X86MCP_SNAPSHOT_CACHE_DIR", snapshot_cache.path());
        }),
    )?;
    let client = ().serve(transport).await?;

    let status = client
        .call_tool(CallToolRequestParams::new("x86_index_status"))
        .await?
        .structured_content
        .expect("structured status output");
    assert_eq!(status["status"]["state"], "degraded_model");
    assert!(status["status"]["reasons"].as_array().unwrap().len() > 1);
    assert!(
        status["status"]["freshness"]
            .as_array()
            .unwrap()
            .iter()
            .all(|archive| archive["fresh"] == true)
    );
    assert_eq!(status["counts"]["documents"], 15);
    assert_eq!(status["counts"]["chunks"], 70_576);
    assert_eq!(status["counts"]["diagrams"], 190);

    let outline = client
        .call_tool(
            CallToolRequestParams::new("x86_get_outline").with_arguments(
                json!({"depth": 0, "limit": 20})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?
        .structured_content
        .expect("structured outline output");
    assert_eq!(outline["documents"].as_array().unwrap().len(), 15);

    let search = client
        .call_tool(
            CallToolRequestParams::new("x86_search").with_arguments(
                json!({
                    "query": "CR4.VMXE virtual machine extensions",
                    "mode": "lexical",
                    "vendors": ["intel"],
                    "limit": 5
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?
        .structured_content
        .expect("structured search output");
    let hits = search["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit["citation"]["vendor"] == "intel"));
    assert!(hits.iter().any(|hit| {
        hit["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("VMX"))
    }));

    client.cancel().await?;
    let cache_generation = snapshot_cache.path().join(&snapshot_id);
    assert!(cache_generation.join("catalog.sqlite3").is_file());
    assert!(cache_generation.join("vectors.f32").is_file());

    Ok(())
}
