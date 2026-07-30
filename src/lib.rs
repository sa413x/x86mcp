use anyhow::Result;

use cli::args::{Cli, Command};

pub mod catalog;
pub mod cli;
pub mod config;
pub mod corpus;
pub mod domain;
pub mod index;
pub mod ingest;
pub mod mcp;
pub mod query;
pub mod setup;
pub mod snapshot;

pub async fn run(cli: Cli) -> Result<u8> {
    match cli.command {
        Command::Setup {
            data_url,
            data_sha256,
            force,
        } => {
            let root = match cli.root {
                Some(root) => root,
                None => config::AppConfig::default_data_root()?,
            };
            let config = config::AppConfig::prepare_root(root)?;
            cli::setup::run(
                &config,
                setup::SetupOptions {
                    data_source: data_url,
                    expected_sha256: data_sha256,
                    force,
                },
            )
            .await
        }
        Command::Index { force } => {
            let config = runtime_config(cli.root)?;
            cli::index::run(&config, force).await
        }
        Command::Status => {
            let config = runtime_config(cli.root)?;
            cli::status::run(&config).await
        }
        Command::Serve => {
            let config = runtime_config(cli.root)?;
            cli::serve::run(&config).await
        }
    }
}

fn runtime_config(root: Option<std::path::PathBuf>) -> Result<config::AppConfig> {
    let root = match root {
        Some(root) => root,
        None => config::AppConfig::default_runtime_root()?,
    };
    config::AppConfig::from_root(root)
}
