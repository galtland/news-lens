//! Wiki command.

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::args::{WikiArgs, WikiCommands};
use crate::config::AppConfig;

#[derive(Debug, Serialize)]
struct WikiStatus {
    path: PathBuf,
    raw_news_count: usize,
    theses_count: usize,
    uncommented_news_count: usize,
}

pub async fn execute(args: WikiArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;

    match args.command {
        WikiCommands::Status => {
            let status = status(&config.wiki.path);
            println!("Wiki: {}", status.path.display());
            println!("Raw news: {}", status.raw_news_count);
            println!("Theses: {}", status.theses_count);
            println!("Uncommented news: {}", status.uncommented_news_count);
        }
    }

    Ok(())
}

fn status(path: &Path) -> WikiStatus {
    let raw_news_count = count_markdown_files(&path.join("raw/news"))
        .or_else(|| count_markdown_files(&path.join("wiki/raw/news")))
        .unwrap_or(0);
    let theses_count = count_markdown_files(&path.join("theses"))
        .or_else(|| count_markdown_files(&path.join("wiki/theses")))
        .unwrap_or(0);
    let uncommented_news_count = raw_news_count.saturating_sub(theses_count);

    WikiStatus {
        path: path.to_path_buf(),
        raw_news_count,
        theses_count,
        uncommented_news_count,
    }
}

fn count_markdown_files(path: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(path).ok()?;
    Some(
        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
            .count(),
    )
}
