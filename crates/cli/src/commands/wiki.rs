//! Wiki command.

use anyhow::{Context, Result};
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
            let status = status(&config.wiki.path)?;
            println!("Wiki: {}", status.path.display());
            println!("Raw news: {}", status.raw_news_count);
            println!("Theses: {}", status.theses_count);
            println!("Raw/thesis delta: {}", status.raw_news_thesis_delta);
        }
    }

    Ok(())
}

fn status(path: &Path) -> Result<WikiStatus> {
    let raw_news_count = count_markdown_files(&path.join("raw/news"))
        .with_context(|| format!("Failed to count raw news under {}", path.display()))?;
    let theses_count = count_markdown_files(&path.join("theses"))
        .with_context(|| format!("Failed to count theses under {}", path.display()))?;
    let raw_news_thesis_delta = raw_news_count as isize - theses_count as isize;

    Ok(WikiStatus {
        path: path.to_path_buf(),
        raw_news_count,
        theses_count,
        raw_news_thesis_delta,
    })
}

fn count_markdown_files(path: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    count_markdown_files_recursive(path, &mut count)?;
    Ok(count)
}

fn count_markdown_files_recursive(path: &Path, count: &mut usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    for entry in entries {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_markdown_files_treats_missing_directory_as_empty() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let count = count_markdown_files(&dir.path().join("missing")).expect("count");

        assert_eq!(count, 0);
    }

    #[test]
    fn count_markdown_files_propagates_read_errors() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "").expect("file");

        let error = count_markdown_files(&file).expect_err("read_dir error");

        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
