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
    raw_news_thesis_delta: isize,
}

pub async fn execute(args: WikiArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;

    match args.command {
        WikiCommands::Status => {
            let status = status(&config.wiki.path);
            println!("Wiki: {}", status.path.display());
            println!("Raw news: {}", status.raw_news_count);
            println!("Theses: {}", status.theses_count);
            println!("Raw/thesis delta: {}", status.raw_news_thesis_delta);
        }
    }

    Ok(())
}

fn status(path: &Path) -> WikiStatus {
    let raw_news_count = count_markdown_files(&path.join("raw/news")).unwrap_or(0);
    let theses_count = count_markdown_files(&path.join("theses")).unwrap_or(0);
    let raw_news_thesis_delta = raw_news_count as isize - theses_count as isize;

    WikiStatus {
        path: path.to_path_buf(),
        raw_news_count,
        theses_count,
        raw_news_thesis_delta,
    }
}

fn count_markdown_files(path: &Path) -> Option<usize> {
    let mut count = 0;
    count_markdown_files_recursive(path, &mut count).ok()?;
    Some(count)
}

fn count_markdown_files_recursive(path: &Path, count: &mut usize) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            count_markdown_files_recursive(&path, count)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            *count += 1;
        }
    }
    Ok(())
}
