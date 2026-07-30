use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use x86mcp::cli::args::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("x86mcp=info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    match x86mcp::run(Cli::parse()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
