# x86mcp

[![MCP Badge](https://lobehub.com/badge/mcp/sa413x-x86mcp)](https://lobehub.com/mcp/sa413x-x86mcp)

A local MCP server for searching Intel SDM and AMD APM manuals. It indexes converted Markdown, handles exact x86 symbols and ordinary text, supports semantic search, and returns citations to the original source.

The server does not read PDFs. Its input is a set of ZIP archives containing `.md` files.

## Install

### npm

With Node.js installed, install the Intel and AMD data once, then let the MCP client start the server with `npx`:

```bash
npx -y x86mcp setup
```

```json
{
  "mcpServers": {
    "x86": {
      "command": "npx",
      "args": ["-y", "x86mcp", "serve"]
    }
  }
}
```

The npm package downloads the native release binary for macOS (Apple silicon and Intel), Linux (x86_64 and ARM64, glibc), or Windows (x86_64).

### Native installer

macOS and Linux:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/sa413x/x86mcp/releases/latest/download/x86mcp-installer.ps1 | iex
```

On macOS, run the command in Terminal. Release binaries are published for Apple silicon (`aarch64-apple-darwin`) and Intel Macs (`x86_64-apple-darwin`), and the installer picks the right one. The installer puts `x86mcp` in `$CARGO_HOME/bin` (`~/.cargo/bin` by default) and then runs `x86mcp setup`; set `X86MCP_SKIP_SETUP=1` for a binary-only install.

### Data

`x86mcp setup` downloads the Intel and AMD data bundle that matches the binary (about 160 MB), verifies its SHA-256 checksum, and prepares the embedding model (about 470 MB). The data goes into the current user's local application data directory. Running `x86mcp setup` again is safe; use `x86mcp setup --force` to replace the installed bundle. A replacement bundle is fully validated before the previous installation is discarded; if validation fails, the previous installation is restored.

| OS | Installed data (`setup`) | Model and snapshot caches |
| --- | --- | --- |
| macOS | `~/Library/Application Support/x86mcp` | `~/Library/Caches/dev.x86mcp.x86mcp` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/x86mcp` | `${XDG_CACHE_HOME:-~/.cache}/x86mcp` |
| Windows | `%LOCALAPPDATA%\x86mcp` | `%LOCALAPPDATA%\x86mcp\x86mcp\cache` |

`serve` also starts before any data is installed. Tools and prompts are listed as usual, the only listed resource is `x86://index/status`, `x86_index_status` reports the `missing` state, and other tools return an error that explains how to install the data. Once `setup` finishes, the running server picks up the data on its next request.

## MCP client setup

The npm configuration above works on every platform. For a native install, point the client at the installed binary. No `--root` argument is needed for the default user installation. MCP client configs do not expand `~` or `$HOME`, so `command` must be an absolute path.

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

Claude Desktop on macOS reads this from `~/Library/Application Support/Claude/claude_desktop_config.json`. Claude Code can register the server for all projects directly:

```bash
claude mcp add --scope user x86 -- npx -y x86mcp serve
claude mcp add --scope user x86 -- "$HOME/.cargo/bin/x86mcp" serve
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

## Prompts

Each prompt tells the model which tools to call, what to cover, and how to cite. `vendor` is `intel` or `amd`; leave it empty to cover both.

| Prompt | Arguments | Produces |
| --- | --- | --- |
| `explain_instruction` | `mnemonic`, optional `vendor` | Operation, opcode and operand forms, flags, exceptions per mode, CPUID enumeration, and Intel/AMD differences of an instruction |
| `describe_register` | `register`, optional `vendor` | Address or encoding, bit fields, access rules, CPUID enumeration, and vendor differences of a control register or MSR |
| `compare_vendors` | `topic` | A cited table of how Intel and AMD document a feature |
| `research` | `question`, optional `vendor` | A cited answer to an open x86 architecture question |

## Resources

| URI | Content |
| --- | --- |
| `x86://index/status` | Index status as JSON, the same as `x86_index_status`; listed even before any data is installed |
| `x86://documents/{document_id}` | One resource per manual volume: its top-level section outline with section IDs, as Markdown |
| `x86://sections/{id}` (template) | A section or block by ID as Markdown, with its citation and subsections and without running page headers and footers; cut at 128 KiB, with a pointer to `x86_get_section` for the rest |

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

## Verification

```bash
cargo test
cargo run --release -- status
```

`status` should report `ready`. Do not ignore `stale` or `semantic_degraded_reason` when an answer depends on the current corpus. `semantic_degraded_reasons` lists model problems even when the state is `stale`; semantic retrieval is disabled while it is non-empty. `status` exits with 0 for `ready` or `degraded_model`, 1 for `stale` or `invalid`, and 2 for `missing` (for example before `x86mcp setup` has run).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

The Intel SDM and AMD APM text in `corpus/` and in the released data bundle is copyrighted by Intel Corporation and Advanced Micro Devices, Inc., respectively, and is not covered by these licenses.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
