use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use anyhow::Result;
use rmcp::{
    RoleClient, ServiceError, ServiceExt,
    model::{
        CallToolRequestParams, ErrorCode, GetPromptRequestParams, JsonObject,
        ReadResourceRequestParams,
    },
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Value, json};
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
const PROMPT_NAMES: [&str; 4] = [
    "compare_vendors",
    "describe_register",
    "explain_instruction",
    "research",
];
const STATUS_URI: &str = "x86://index/status";

type Client = RunningService<RoleClient, ()>;

/// Spawns `x86mcp serve` on `root` with model and snapshot caches under `caches`.
async fn serve(root: &Path, caches: &Path) -> Result<Client> {
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_x86mcp")).configure(|command| {
            command
                .arg("--root")
                .arg(root)
                .arg("serve")
                .env("FASTEMBED_CACHE_DIR", caches.join("models"))
                .env("X86MCP_SNAPSHOT_CACHE_DIR", caches.join("snapshots"));
        }),
    )?;
    Ok(().serve(transport).await?)
}

fn arguments(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

async fn call(client: &Client, tool: &'static str, arguments: Value) -> Result<Value> {
    let mut request = CallToolRequestParams::new(tool);
    if let Value::Object(arguments) = arguments {
        request = request.with_arguments(arguments);
    }
    Ok(client
        .call_tool(request)
        .await?
        .structured_content
        .expect("structured tool output"))
}

async fn read_text(client: &Client, uri: &str) -> Result<String> {
    let result = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await?;
    Ok(serde_json::to_value(result)?["contents"][0]["text"]
        .as_str()
        .expect("text resource")
        .to_owned())
}

async fn prompt_text(client: &Client, name: &str, arguments: Value) -> Result<String> {
    let result = client
        .get_prompt(GetPromptRequestParams::new(name).with_arguments(self::arguments(arguments)))
        .await?;
    Ok(
        serde_json::to_value(result)?["messages"][0]["content"]["text"]
            .as_str()
            .expect("text prompt message")
            .to_owned(),
    )
}

fn error_code(error: ServiceError) -> ErrorCode {
    match error {
        ServiceError::McpError(error) => error.code,
        other => panic!("expected an MCP error, got {other}"),
    }
}

#[tokio::test]
async fn stdio_server_starts_without_data_and_picks_up_a_later_install() -> Result<()> {
    let root = tempfile::tempdir()?;
    let caches = tempfile::tempdir()?;
    let client = serve(root.path(), caches.path()).await?;

    let mut tools = client.list_all_tools().await?;
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        TOOL_NAMES
    );
    let mut prompts = client.list_all_prompts().await?;
    prompts.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(
        prompts
            .iter()
            .map(|prompt| prompt.name.as_str())
            .collect::<Vec<_>>(),
        PROMPT_NAMES
    );
    let resources = client.list_all_resources().await?;
    assert_eq!(
        resources
            .iter()
            .map(|resource| resource.uri.as_str())
            .collect::<Vec<_>>(),
        [STATUS_URI]
    );
    let templates = client.list_all_resource_templates().await?;
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].uri_template, "x86://sections/{id}");

    let status = call(&client, "x86_index_status", Value::Null).await?;
    assert_eq!(status["status"]["state"], "missing");
    assert!(status["manifest"].is_null());
    assert!(status["counts"].is_null());
    let status: Value = serde_json::from_str(&read_text(&client, STATUS_URI).await?)?;
    assert_eq!(status["status"]["state"], "missing");

    let search = json!({"query": "VMX", "mode": "lexical", "limit": 5});
    let error = client
        .call_tool(
            CallToolRequestParams::new("x86_search").with_arguments(arguments(search.clone())),
        )
        .await
        .expect_err("retrieval needs an installed index");
    assert!(error.to_string().contains("x86mcp setup"));

    build_fixture(root.path());
    let hits = call(&client, "x86_search", search).await?;
    assert!(!hits["hits"].as_array().unwrap().is_empty());
    assert_eq!(client.list_all_resources().await?.len(), 3);

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn stdio_server_reads_manual_resources() -> Result<()> {
    let root = tempfile::tempdir()?;
    let caches = tempfile::tempdir()?;
    build_fixture(root.path());
    let client = serve(root.path(), caches.path()).await?;

    let resources = client.list_all_resources().await?;
    let documents = resources
        .iter()
        .filter(|resource| resource.uri.starts_with("x86://documents/doc:"))
        .collect::<Vec<_>>();
    assert_eq!(documents.len(), 2);
    let intel = documents
        .iter()
        .find(|resource| resource.name == "intel")
        .expect("intel manual resource");
    let outline = read_text(&client, &intel.uri).await?;
    assert!(outline.contains("- Intel Manual (`sec:"));

    let hits = call(
        &client,
        "x86_search",
        json!({"query": "VMX", "mode": "lexical", "limit": 5}),
    )
    .await?;
    let section_id = hits["hits"][0]["citation"]["section_id"].as_str().unwrap();
    // URI template clients percent-encode the ':' inside section IDs.
    let encoded = format!("x86://sections/{}", section_id.replace(':', "%3A"));
    let section = read_text(&client, &encoded).await?;
    assert!(section.contains("CR4.VMXE enables virtual-machine extensions."));
    assert_eq!(
        read_text(&client, &format!("x86://sections/{section_id}")).await?,
        section
    );

    let status: Value = serde_json::from_str(&read_text(&client, STATUS_URI).await?)?;
    assert_eq!(status["counts"]["documents"], 2);

    for uri in [
        "x86://sections/sec:unknown",
        "x86://sections/%zz",
        "x86://tables/tbl:1",
    ] {
        let error = client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .expect_err("unknown resources are errors");
        assert_eq!(error_code(error), ErrorCode::RESOURCE_NOT_FOUND, "{uri}");
    }

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn stdio_server_prompts_validate_and_scope_arguments() -> Result<()> {
    let root = tempfile::tempdir()?;
    let caches = tempfile::tempdir()?;
    let client = serve(root.path(), caches.path()).await?;

    let prompts = client.list_all_prompts().await?;
    let explain = prompts
        .iter()
        .find(|prompt| prompt.name == "explain_instruction")
        .unwrap();
    let required = explain
        .arguments
        .iter()
        .flatten()
        .map(|argument| (argument.name.as_str(), argument.required))
        .collect::<Vec<_>>();
    assert!(required.contains(&("mnemonic", Some(true))));
    assert!(required.contains(&("vendor", Some(false))));

    let amd = prompt_text(
        &client,
        "explain_instruction",
        json!({"mnemonic": " VMRUN ", "vendor": "AMD"}),
    )
    .await?;
    assert!(amd.contains(r#""entity":"VMRUN""#));
    assert!(amd.contains(r#""vendors":["amd"]"#));
    // Clients send unfilled optional arguments as empty strings.
    let both = prompt_text(
        &client,
        "explain_instruction",
        json!({"mnemonic": "VMRUN", "vendor": ""}),
    )
    .await?;
    assert!(!both.contains(r#""vendors""#));

    for (name, arguments) in [
        (
            "explain_instruction",
            json!({"mnemonic": "VMRUN", "vendor": "arm"}),
        ),
        ("explain_instruction", json!({"mnemonic": "  "})),
        ("describe_register", json!({})),
    ] {
        let error = client
            .get_prompt(
                GetPromptRequestParams::new(name).with_arguments(self::arguments(arguments)),
            )
            .await
            .expect_err("invalid prompt arguments are errors");
        assert_eq!(error_code(error), ErrorCode::INVALID_PARAMS, "{name}");
    }

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn stdio_server_lists_ten_typed_tools_and_serves_structured_results() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let caches = tempfile::tempdir()?;
    build_fixture(temporary.path());
    let client = serve(temporary.path(), caches.path()).await?;

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
