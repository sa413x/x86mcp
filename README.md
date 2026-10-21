# x86mcp

A local MCP server for searching Intel SDM and AMD APM manuals. It indexes converted Markdown, handles exact x86 symbols and ordinary text, supports semantic search, and returns citations to the original source.

The server does not read PDFs. Its input is a set of ZIP archives containing `.md` files.

## Install

macOS and Linux:

```bash
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"; curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.sh | sh && "$CARGO_HOME/bin/x86mcp" setup
```

Windows PowerShell:

```powershell
irm https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.ps1 | iex; & "$(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$HOME\.cargo" })\bin\x86mcp.exe" setup
```

On macOS, run the command in Terminal. Release binaries are published for Apple silicon (`aarch64-apple-darwin`) and Intel Macs (`x86_64-apple-darwin`), and the installer picks the right one.

The command installs a native binary, downloads the matching Intel and AMD data bundle, verifies its SHA-256 checksum, and prepares the embedding model. The data goes into the current user's local application data directory. Running `x86mcp setup` again is safe; use `x86mcp setup --force` to replace the installed bundle. A replacement bundle is fully validated before the previous installation is discarded; if validation fails, the previous installation is restored.

| OS | Installed data (`setup`) | Model and snapshot caches |
| --- | --- | --- |
| macOS | `~/Library/Application Support/x86mcp` | `~/Library/Caches/dev.x86mcp.x86mcp` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/x86mcp` | `${XDG_CACHE_HOME:-~/.cache}/x86mcp` |
| Windows | `%LOCALAPPDATA%\x86mcp` | `%LOCALAPPDATA%\x86mcp\x86mcp\cache` |

## Build from source

You need a stable Rust toolchain with edition 2024 support and a C compiler for the bundled SQLite, zstd, and Oniguruma sources. On macOS, install the Xcode Command Line Tools first:

```bash
xcode-select --install
```

```bash
cargo build --release
cargo run --release -- index
cargo run --release -- status
cargo run --release -- serve
```

The first `index` run downloads the embedding model, then builds the catalog, BM25 index, and vector index. Later runs reuse unchanged parsed documents and embeddings (embeddings only when the model artifacts are identical). Concurrent `index` runs on the same root wait for each other. To rebuild everything, re-parsing and re-embedding every document and replacing the current snapshot even when its ID is unchanged:

```bash
cargo run --release -- index --force
```

`serve` runs MCP over stdio. Logs go to stderr and do not interfere with the protocol stream.

## Corpus layout

x86mcp reads `corpus/` and `index/` from its root. When the current directory is a source checkout (it contains `corpus/manifest.toml`), the root is the checkout and `index` builds the snapshot there if none exists yet; otherwise the root is the data directory installed by `x86mcp setup`. The corpus looks like this:

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

After installation and `x86mcp setup`, point the client at the installed binary. No `--root` argument is needed for the default user installation. MCP client configs do not expand `~` or `$HOME`, so `command` must be an absolute path.

macOS (on Linux the path is usually `/home/you/.cargo/bin/x86mcp`):

```json
{
  "mcpServers": {
    "x86": {
      "command": "/Users/you/.cargo/bin/x86mcp",
      "args": ["serve"]
    }
  }
}
```

Claude Desktop on macOS reads this from `~/Library/Application Support/Claude/claude_desktop_config.json`. Claude Code can register the server for all projects directly:

```bash
claude mcp add --scope user x86 -- "$HOME/.cargo/bin/x86mcp" serve
```

Windows:

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

For a source checkout or custom data directory, add `--root` before `serve`, for example `["--root", "/Users/you/src/x86mcp", "serve"]` on macOS or `["--root", "C:\\path\\to\\x86mcp", "serve"]` on Windows. Call `x86_index_status` before retrieval when corpus freshness matters; it re-checks archive freshness and model files on every call, and every retrieval response carries the current `stale` flag.

## Tools

| Tool | Purpose |
| --- | --- |
| `x86_search` | Exact, BM25, semantic, or hybrid search with citations and stable pagination |
| `x86_lookup` | Find an instruction, MSR, CPUID leaf, register, exception, bitfield, or term; typed kinds report `found` only with exact evidence of that kind |
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

`status` should report `ready`. Do not ignore `stale` or `semantic_degraded_reason` when an answer depends on the current corpus. `semantic_degraded_reasons` lists model problems even when the state is `stale`; semantic retrieval is disabled while it is non-empty. `status` exits with 0 for `ready` or `degraded_model`, 1 for `stale` or `invalid`, and 2 for `missing` (for example before `x86mcp setup` has run).
