use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "x86mcp",
    version,
    about = "Cited Intel SDM and AMD APM retrieval over MCP"
)]
pub struct Cli {
    #[arg(long, env = "X86MCP_ROOT", value_name = "PATH")]
    pub root: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Index {
        #[arg(long)]
        force: bool,
    },
    Setup {
        #[arg(long, env = "X86MCP_DATA_URL", value_name = "URL_OR_PATH")]
        data_url: Option<String>,
        #[arg(long, env = "X86MCP_DATA_SHA256", value_name = "SHA256")]
        data_sha256: Option<String>,
        #[arg(long)]
        force: bool,
    },
    Status,
    Serve,
}
