//! Lens command.

use anyhow::{Context, Result, bail};
use news_lens_adapters::lens::load_lens;
use news_lens_domain::Lens;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::args::{LensArgs, LensCommands};
use crate::config::AppConfig;

pub async fn execute(args: LensArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;

    match args.command {
        LensCommands::List => {
            for lens in discover_lenses(&config.lens.path)? {
                println!(
                    "{}\t{}\t{}\t{}",
                    lens.id,
                    lens.path.display(),
                    lens.voice.as_deref().unwrap_or(""),
                    lens.register.as_deref().unwrap_or("")
                );
            }
        }
        LensCommands::Show { id } => {
            let lens = discover_lenses(&config.lens.path)?
                .into_iter()
                .find(|lens| lens.id == id)
                .with_context(|| format!("Lens not found: {}", id))?;
            println!("{}", lens.content);
        }
    }

    Ok(())
}

fn discover_lenses(configured_path: &Path) -> Result<Vec<Lens>> {
    let mut lenses = HashMap::new();

    if configured_path.is_file() {
        let lens = load_lens(configured_path)?;
        lenses.insert(lens.id.clone(), lens);
    }
    let configured_canonical = configured_path.canonicalize().ok();

    let parent = configured_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    if !parent.exists() {
        if lenses.is_empty() {
            bail!("Lens directory does not exist: {}", parent.display());
        }
        let mut lenses = lenses.into_values().collect::<Vec<_>>();
        lenses.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(lenses);
    }

    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        let is_configured_path = configured_canonical
            .as_ref()
            .is_some_and(|configured| path.canonicalize().ok().as_ref() == Some(configured));
        if !path.is_file() || is_configured_path {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            match load_lens(&path) {
                Ok(lens) => {
                    lenses.entry(lens.id.clone()).or_insert(lens);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "Skipping invalid lens file"
                    );
                }
            }
        }
    }

    let mut lenses = lenses.into_values().collect::<Vec<_>>();
    lenses.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(lenses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_lenses_treats_bare_relative_parent_as_current_directory() {
        discover_lenses(Path::new("lens.md")).expect("bare relative path");
    }

    #[test]
    fn discover_lenses_prefers_configured_path_on_duplicate_id() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let configured_path = dir.path().join("configured.md");
        let scanned_path = dir.path().join("scanned.md");

        std::fs::write(
            &configured_path,
            "---\nid: duplicate\nvoice: configured\n---\n\n# Configured\n",
        )
        .expect("configured lens");
        std::fs::write(
            &scanned_path,
            "---\nid: duplicate\nvoice: scanned\n---\n\n# Scanned\n",
        )
        .expect("scanned lens");

        let lenses = discover_lenses(&configured_path).expect("lenses");

        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].path, configured_path);
        assert_eq!(lenses[0].voice.as_deref(), Some("configured"));
    }
}
