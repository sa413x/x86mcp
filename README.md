# x86mcp

A local MCP server for searching Intel SDM and AMD APM manuals. It indexes converted Markdown, handles exact x86 symbols and ordinary text, supports semantic search, and returns citations to the original source.

The server does not read PDFs. Its input is a set of ZIP archives containing `.md` files.

## Install

Linux and macOS:

```bash
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"; curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.sh | sh && "$CARGO_HOME/bin/x86mcp" setup
```

Windows PowerShell:

```powershell
irm https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.ps1 | iex; & "$HOME\.cargo\bin\x86mcp.exe" setup
```

The command installs a native binary, downloads the matching Intel and AMD data bundle, verifies its SHA-256 checksum, and prepares the embedding model. The data goes into the current user's local application data directory. Running `x86mcp setup` again is safe; use `x86mcp setup --force` to replace the installed bundle.

## Build from source

You need a stable Rust toolchain with edition 2024 support.

```bash
cargo build --release
cargo run --release -- index
cargo run --release -- status
cargo run --release -- serve
```

The first `index` run downloads the embedding model, then builds the catalog, BM25 index, and vector index. Later runs reuse unchanged data. To rebuild everything:

```bash
cargo run --release -- index --force
```

`serve` runs MCP over stdio. Logs go to stderr and do not interfere with the protocol stream.

## Corpus layout

In a source checkout, x86mcp uses `./corpus` and `./index` when both are present:

```text
corpus/
  manifest.toml
  intel-sdm-md.zip
  amd-apm-md.zip
```

Each archive entry in `manifest.toml` needs an `id`, `vendor`, `path`, SHA-256 checksum, Markdown file count, and total uncompressed size. Checksums are mandatory. If an archive changes or is damaged, indexing stops instead of building a bad snapshot.

Use the global `--root` argument or `X86MCP_ROOT` to point at another installation:

```bash
x86mcp --root C:/data/x86-manuals serve
```

Set `FASTEMBED_CACHE_DIR` to move the model cache. Set `X86MCP_SNAPSHOT_CACHE_DIR` to move the unpacked snapshot cache.

## MCP client setup

After installation and `x86mcp setup`, point the client at the installed binary. No `--root` argument is needed for the default user installation.

```json
{
  "mcpServers": {
    "x86": {
      "command": "C:\\Users\\you\\.cargo\\bin\\x86mcp.exe",
      "args": ["serve"]
    }
  }
}
```

For a source checkout or custom data directory, add `["--root", "C:\\path\\to\\x86mcp", "serve"]`. Call `x86_index_status` before retrieval when corpus freshness matters.

## Tools

| Tool | Purpose |
| --- | --- |
| `x86_search` | Exact, BM25, semantic, or hybrid search with citations and stable pagination |
| `x86_lookup` | Find an instruction, MSR, CPUID leaf, register, exception, bitfield, or term |
| `x86_get_section` | Read a manual section with its blocks, tables, and diagrams |
| `x86_get_outline` | Walk the document and section hierarchy |
| `x86_get_table` | Read a normalized table, filter rows, or inspect the original markup |
| `x86_get_diagram` | Read a Mermaid or text diagram with nodes, edges, and raw source |
| `x86_get_references` | Follow incoming and outgoing references between manual entities |
| `x86_compare_vendors` | Run one query against Intel and AMD with a separate limit for each vendor |
| `x86_build_context` | Build a citation-preserving evidence pack within a token budget |
| `x86_index_status` | Check the snapshot, archive freshness, model state, and index counts |

Example `x86_search` arguments:

```json
{
  "query": "IA32_EFER nested paging",
  "mode": "hybrid",
  "vendors": ["amd"],
  "document_ids": [],
  "kinds": [],
  "limit": 10
}
```

Responses include the snapshot state and source citations. If the semantic model is unavailable, hybrid search reports the reason and continues with exact and BM25 retrieval.

## Verification

```bash
cargo test
cargo run --release -- status
```

`status` should report `ready`. Do not ignore `stale` or `semantic_degraded_reason` when an answer depends on the current corpus.
