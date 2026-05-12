//! news-lens CLI entry point

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod args;
mod commands;
mod config;

use args::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = cli
        .log_level
        .clone()
        .or_else(|| config::AppConfig::log_level_from_config(cli.config.as_deref()))
        .unwrap_or_else(|| "info".to_string());
    init_logging(&log_level)?;

    // Execute command
    match cli.command {
        Commands::Run(args) => commands::run::execute(args, cli.config).await,
        Commands::Process(args) => commands::process::execute(args, cli.config).await,
        Commands::Wiki(args) => commands::wiki::execute(args, cli.config).await,
        Commands::Lens(args) => commands::lens::execute(args, cli.config).await,
        Commands::Config(args) => commands::config::execute(args).await,
        Commands::Doctor(args) => commands::doctor::execute(args, cli.config).await,
    }
}

fn init_logging(level: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(level))?;

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
        .with(filter)
        .init();

    Ok(())
}
