use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use anyhow::Result;
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use x86mcp::{
    config::AppConfig,
    index::{Embedder, ModelSpec},
    snapshot::SnapshotBuilder,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const INTEL: &str =
    "# Intel Manual\n\n## VMX Operation\n\nCR4.VMXE enables virtual-machine extensions.\n";
const AMD: &str = "# AMD Manual\n\n## SVM Operation\n\nVMRUN enters an AMD guest.\n";
const TOOL_NAMES: [&str; 10] = [
    "x86_build_context",
    "x86_compare_vendors",
    "x86_get_diagram",
    "x86_get_outline",
    "x86_get_references",
    "x86_get_section",
    "x86_get_table",
    "x86_index_status",
    "x86_lookup",
    "x86_search",
];

#[tokio::test]
async fn stdio_server_lists_ten_typed_tools_and_serves_structured_results() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    build_fixture(temporary.path());

    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_x86mcp")).configure(|command| {
            command.arg("--root").arg(temporary.path()).arg("serve");
        }),
    )?;
    let client = ().serve(transport).await?;

    let mut tools = client.list_all_tools().await?;
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        TOOL_NAMES
    );
    assert!(tools.iter().all(|tool| tool.output_schema.is_some()));
    assert!(tools.iter().all(|tool| {
        tool.annotations
            .as_ref()
            .and_then(|annotations| annotations.read_only_hint)
            == Some(true)
    }));

    let status = client
        .call_tool(CallToolRequestParams::new("x86_index_status"))
        .await?;
    let status = status.structured_content.expect("structured status output");
    assert_eq!(status["manifest"]["counts"]["documents"], 2);
    assert_eq!(status["status"]["freshness"].as_array().unwrap().len(), 2);

    let search = client
        .call_tool(
            CallToolRequestParams::new("x86_search").with_arguments(
                json!({"query": "VMX", "mode": "lexical", "limit": 5})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let search = search.structured_content.expect("structured search output");
    assert!(!search["hits"].as_array().unwrap().is_empty());
    assert_eq!(search["hits"][0]["citation"]["vendor"], "intel");
    assert!(search["hits"][0]["citation"]["entry_path"].is_string());
    assert!(
        search["hits"][0]["citation"]["span"]["byte_end"]
            .as_u64()
            .unwrap()
            > 0
    );

    let error = client
        .call_tool(
            CallToolRequestParams::new("x86_search").with_arguments(
                json!({"query": "VMX", "mode": "lexical", "limit": 0})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect_err("invalid limits must be protocol errors");
    assert!(error.to_string().contains("between 1 and 50"));

    client.cancel().await?;
    Ok(())
}

struct FixtureEmbedder {
    spec: ModelSpec,
}

impl FixtureEmbedder {
    fn new() -> Self {
        Self {
            spec: ModelSpec {
                code: "mcp-fixture".into(),
                dimension: 4,
                max_tokens: 512,
                query_prefix: String::new(),
                passage_prefix: String::new(),
            },
        }
    }
}

impl Embedder for FixtureEmbedder {
    fn spec(&self) -> &ModelSpec {
        &self.spec
    }
    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(text.split_whitespace().count())
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| vector(text)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(vector(text))
    }
}

fn vector(text: &str) -> Vec<f32> {
    let mut values = vec![0.0_f32; 4];
    for (index, byte) in text.bytes().enumerate() {
        values[index % 4] += f32::from(byte) + 1.0;
    }
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    values.iter_mut().for_each(|value| *value /= norm);
    values
}

fn build_fixture(root: &Path) {
    let corpus_dir = root.join("corpus");
    fs::create_dir_all(&corpus_dir).unwrap();
    fs::create_dir_all(root.join("models")).unwrap();
    let intel = corpus_dir.join("intel.zip");
    let amd = corpus_dir.join("amd.zip");
    write_zip(&intel, "intel.md", INTEL.as_bytes());
    write_zip(&amd, "amd.md", AMD.as_bytes());
    let manifest = format!(
        "schema_version = 1\n\n[[archives]]\nid = \"intel\"\nvendor = \"intel\"\npath = \"intel.zip\"\nsha256 = \"{}\"\nentry_count = 1\nuncompressed_bytes = {}\n\n[[archives]]\nid = \"amd\"\nvendor = \"amd\"\npath = \"amd.zip\"\nsha256 = \"{}\"\nentry_count = 1\nuncompressed_bytes = {}\n",
        sha256(&intel),
        INTEL.len(),
        sha256(&amd),
        AMD.len()
    );
    fs::write(corpus_dir.join("manifest.toml"), manifest).unwrap();
    let config = AppConfig::from_root(root).unwrap();
    SnapshotBuilder::new(&config, &FixtureEmbedder::new())
        .build(false)
        .unwrap();
}

fn write_zip(path: &Path, entry: &str, bytes: &[u8]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    writer
        .start_file(
            entry,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap();
}

fn sha256(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}
