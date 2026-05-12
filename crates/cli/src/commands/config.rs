//! Config command - configuration management

use anyhow::{Context, Result};
use std::fs;

use crate::args::{ConfigArgs, ConfigCommands};
use crate::config::AppConfig;

pub async fn execute(args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigCommands::Init { path, force } => init_config(path, force).await,
    }
}

async fn init_config(path: std::path::PathBuf, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "Config file already exists: {}. Use --force to overwrite.",
            path.display()
        );
    }

    let content = AppConfig::example_toml();

    // Create parent directories if needed
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
    }

    fs::write(&path, content)
        .with_context(|| format!("Failed to write config file: {}", path.display()))?;

    println!("Created config file: {}", path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit the wiki, lens, harness, and account settings");
    println!("  2. Run 'news-lens doctor' to validate your setup");
    println!("  3. Run 'news-lens process --post --text \"Test news item\" --dry-run' to test");

    Ok(())
}
