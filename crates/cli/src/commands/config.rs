//! Config command - configuration management

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::args::{ConfigArgs, ConfigCommands};
use crate::config::{AppConfig, SHIPPED_PROCESS_PROMPT};

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

    // Create parent directories if needed
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
    }

    let prompt_path = prompt_path_for_config(&path)?;
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create prompt directory: {}", parent.display()))?;
    }
    if !prompt_path.exists() {
        fs::write(&prompt_path, SHIPPED_PROCESS_PROMPT).with_context(|| {
            format!("Failed to write prompt template: {}", prompt_path.display())
        })?;
    }

    let content = AppConfig::example_toml_with_prompt_template(&prompt_path);

    fs::write(&path, content)
        .with_context(|| format!("Failed to write config file: {}", path.display()))?;

    println!("Created config file: {}", path.display());
    println!("Prompt template: {}", prompt_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit the wiki, lens, harness, and account settings");
    println!("  2. Run 'news-lens doctor' to validate your setup");
    println!("  3. Run 'news-lens process --post --text \"Test news item\" --dry-run' to test");

    Ok(())
}

fn prompt_path_for_config(config_path: &Path) -> Result<PathBuf> {
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let absolute_parent = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Failed to read current directory")?
            .join(parent)
    };

    Ok(absolute_parent.join("prompts").join("process-post.md"))
}
