//! Gaps command — list wiki-coverage gaps reported by the agent during processing.

use anyhow::{Context, Result};
use news_lens_adapters::state::SqliteStateStore;
use news_lens_domain::{ProcessedPostRecord, StateStore};
use serde::Serialize;
use std::path::PathBuf;

use crate::args::GapsArgs;
use crate::config::AppConfig;

#[derive(Debug, Serialize)]
struct GapEntry<'a> {
    post_id: &'a str,
    lens_id: &'a str,
    processed_at: String,
    stance: &'a str,
    thesis_slug: Option<&'a str>,
    gap: &'a str,
}

pub async fn execute(args: GapsArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;
    let store = SqliteStateStore::new(&config.general.state_db_path)
        .await
        .context("Failed to initialize SQLite state store")?;

    let lens_filter: Option<&str> = if args.all_lenses {
        None
    } else {
        Some(args.lens.as_deref().unwrap_or(config.lens.id.as_str()))
    };

    let records = store
        .list_processed_with_gaps(lens_filter, args.limit)
        .await
        .context("Failed to read gaps from state store")?;

    let total_gaps: usize = records
        .iter()
        .map(|r| r.gaps.as_ref().map_or(0, |g| g.len()))
        .sum();

    if args.json {
        let entries = records
            .iter()
            .flat_map(|r| {
                r.gaps
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(move |gap| GapEntry {
                        post_id: &r.post_id,
                        lens_id: &r.lens_id,
                        processed_at: r.processed_at.to_string(),
                        stance: r.stance.as_str(),
                        thesis_slug: r.thesis_slug.as_deref(),
                        gap,
                    })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if records.is_empty() {
        match lens_filter {
            Some(lens) => println!("No gaps recorded for lens '{lens}'."),
            None => println!("No gaps recorded."),
        }
        return Ok(());
    }

    println!(
        "{total_gaps} gap(s) across {n} post(s){scope}:",
        n = records.len(),
        scope = match lens_filter {
            Some(lens) => format!(" (lens: {lens})"),
            None => " (all lenses)".to_string(),
        }
    );
    for record in &records {
        let Some(gaps) = record.gaps.as_deref() else {
            continue;
        };
        print_record_header(record);
        for gap in gaps {
            println!("    - {gap}");
        }
    }

    Ok(())
}

fn print_record_header(record: &ProcessedPostRecord) {
    let thesis = record.thesis_slug.as_deref().unwrap_or("-");
    println!(
        "\n[{date}] {post_id} ({stance}, lens={lens}, thesis={thesis})",
        date = record.processed_at,
        post_id = record.post_id,
        stance = record.stance.as_str(),
        lens = record.lens_id,
    );
}
