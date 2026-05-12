//! Lens command.

use anyhow::{Context, Result, bail};
use news_lens_adapters::lens::load_lens;
use news_lens_domain::Lens;
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
    let mut lenses = Vec::new();

    if configured_path.is_file() {
        lenses.push(load_lens(configured_path)?);
    }

    let Some(parent) = configured_path.parent() else {
        return Ok(lenses);
    };

    if !parent.exists() {
        if lenses.is_empty() {
            bail!("Lens directory does not exist: {}", parent.display());
        }
        return Ok(lenses);
    }

    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path == configured_path {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            if let Ok(lens) = load_lens(&path) {
                lenses.push(lens);
            }
        }
    }

    lenses.sort_by(|a, b| a.id.cmp(&b.id));
    lenses.dedup_by(|a, b| a.id == b.id);
    Ok(lenses)
}
