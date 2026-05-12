//! Process command.

use anyhow::{Context, Result, bail};
use news_lens_adapters::{
    jsonl::JsonlPostSource,
    state::{InMemoryStateStore, SqliteStateStore},
};
use news_lens_domain::{
    Harness, PostSource, ProcessResult, SourcePost, StateStore, SystemClock,
    usecases::{RunLoop, RunLoopConfig, candidate_slug},
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use time::OffsetDateTime;

use crate::args::ProcessArgs;
use crate::commands::common::{
    build_harness, build_publishers, build_x_post_source, load_configured_lens,
};
use crate::config::AppConfig;

pub async fn execute(args: ProcessArgs, config_path: Option<PathBuf>) -> Result<()> {
    if args.post.is_none() && args.jsonl.is_none() {
        bail!("Expected --post or --jsonl");
    }

    let config = AppConfig::load(config_path.as_deref())?;

    if let Some(post_arg) = args.post {
        let lens = load_configured_lens(&config)?;
        let harness = build_harness(&config)?;
        if args.dry_run {
            tracing::info!(
                "--dry-run is implicit for process --post; no state or publishing occurs"
            );
        }
        let post = resolve_single_post(post_arg, args.text, &config).await?;
        let candidate_slug = candidate_slug(&post);
        let ctx = news_lens_domain::PostContext {
            post,
            wiki_path: config.wiki.path.clone(),
            lens,
            candidate_slug,
        };
        let output = harness
            .process_post(ctx)
            .await
            .context("Harness processing failed")?;
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if let Some(path) = args.jsonl {
        let lens = load_configured_lens(&config)?;
        let harness = build_harness(&config)?;
        let mut state_dry_run = args.dry_run || config.general.dry_run;
        if args.require_approval && state_dry_run {
            tracing::info!(
                "--require-approval overrides configured dry-run for JSONL state writes"
            );
            state_dry_run = false;
        }

        let source = JsonlPostSource::new(vec![path]);
        let posts = source
            .fetch_posts("*", None)
            .await
            .context("Failed to load JSONL posts")?;
        let state_store: Arc<dyn StateStore> = if state_dry_run {
            Arc::new(InMemoryStateStore::new())
        } else {
            Arc::new(
                SqliteStateStore::new(&config.general.state_db_path)
                    .await
                    .context("Failed to initialize SQLite state store")?,
            )
        };
        // `process --jsonl` never publishes directly in v1. Approval mode routes
        // commentary to the outbox; otherwise publishers stay disabled.
        let outbox_dry_run = !args.require_approval;
        let (x_publisher, nostr_publisher) =
            build_publishers(&config, outbox_dry_run, args.require_approval, args.outbox).await?;
        let run_loop = RunLoop::new(
            Arc::new(source),
            Arc::new(harness),
            x_publisher,
            nostr_publisher,
            state_store,
            Arc::new(SystemClock),
            RunLoopConfig {
                accounts: vec![],
                include_replies: config.watch.include_replies,
                include_reposts: config.watch.include_reposts,
                ignore_patterns: config.watch.ignore_patterns.clone(),
                dry_run: outbox_dry_run,
                wiki_path: config.wiki.path.clone(),
                lens,
            },
        );
        let results = run_loop.process_posts(posts).await?;
        for result in results_to_json(results) {
            println!("{}", serde_json::to_string(&result)?);
        }
        return Ok(());
    }

    unreachable!("process argument shape was checked before loading config")
}

async fn resolve_single_post(
    post_arg: Option<String>,
    text_arg: Option<String>,
    config: &AppConfig,
) -> Result<SourcePost> {
    match text_arg {
        Some(text) => synthetic_post(post_arg, text),
        None => {
            let Some(value) = post_arg else {
                bail!("--post requires a value or --text")
            };

            if should_lookup_source_id(&value) {
                match fetch_source_post_by_id(&value, config).await {
                    Ok(Some(post)) => return Ok(post),
                    Ok(None) if looks_like_x_post_id(&value) => {
                        bail!(
                            "No configured source post found with id {}; use --text to process this value literally",
                            value
                        );
                    }
                    Ok(None) => {}
                    Err(error) if looks_like_x_post_id(&value) => {
                        bail!(
                            "Failed to fetch source post id {}; use --text to process this value literally: {:#}",
                            value,
                            error
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            post_arg = %value,
                            "Source post lookup failed; treating --post value as ad-hoc text. Pass --text to skip lookup"
                        );
                    }
                }
            }

            synthetic_post(None, value)
        }
    }
}

async fn fetch_source_post_by_id(post_id: &str, config: &AppConfig) -> Result<Option<SourcePost>> {
    let source = build_x_post_source(config).context("Failed to initialize X post lookup")?;
    source
        .fetch_post_by_id(post_id)
        .await
        .with_context(|| format!("Failed to fetch source post id {}", post_id))
}

fn should_lookup_source_id(value: &str) -> bool {
    looks_like_x_post_id(value)
}

fn looks_like_x_post_id(value: &str) -> bool {
    value.len() >= 10 && value.chars().all(|ch| ch.is_ascii_digit())
}

fn synthetic_post(post_id: Option<String>, text: String) -> Result<SourcePost> {
    let id = match post_id {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        Some(_) | None => "cli-input".to_string(),
    };

    if text.trim().is_empty() {
        bail!("post text is empty");
    }

    Ok(SourcePost {
        id,
        text,
        author: "cli".to_string(),
        url: String::new(),
        created_at: OffsetDateTime::now_utc(),
        is_repost: false,
        is_reply: false,
        reply_to_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_post_uses_explicit_id_when_text_is_provided() {
        let post = synthetic_post(Some("source-123".to_string()), "body".to_string())
            .expect("synthetic post");

        assert_eq!(post.id, "source-123");
        assert_eq!(post.text, "body");
    }

    #[test]
    fn synthetic_post_trims_explicit_id() {
        let post = synthetic_post(Some("  source-123  ".to_string()), "body".to_string())
            .expect("synthetic post");

        assert_eq!(post.id, "source-123");
    }

    #[test]
    fn synthetic_post_defaults_id_when_only_text_is_provided() {
        let post = synthetic_post(None, "body".to_string()).expect("synthetic post");

        assert_eq!(post.id, "cli-input");
        assert_eq!(post.text, "body");
    }

    #[test]
    fn x_post_id_heuristic_requires_digits_only() {
        assert!(looks_like_x_post_id("1234567890"));
        assert!(!looks_like_x_post_id("42"));
        assert!(!looks_like_x_post_id("fixture-1"));
        assert!(!looks_like_x_post_id("short headline"));
    }

    #[test]
    fn source_lookup_requires_x_post_id_shape() {
        assert!(should_lookup_source_id("1234567890"));
        assert!(!should_lookup_source_id("fixture-1"));
        assert!(!should_lookup_source_id("my headline"));
    }
}

#[derive(Serialize)]
struct JsonProcessResult {
    post_id: String,
    stance: String,
    raw_path: Option<String>,
    thesis_slug: Option<String>,
    x_post_id: Option<String>,
    nostr_event_id: Option<String>,
    error: Option<String>,
}

fn results_to_json(results: Vec<(String, ProcessResult)>) -> Vec<JsonProcessResult> {
    results
        .into_iter()
        .map(|(post_id, result)| match result {
            ProcessResult::Processed {
                agent_return,
                x_post_id,
                nostr_event_id,
                ..
            } => JsonProcessResult {
                post_id,
                stance: agent_return.stance.to_string(),
                raw_path: Some(agent_return.raw_path),
                thesis_slug: agent_return.thesis_slug,
                x_post_id,
                nostr_event_id,
                error: None,
            },
            ProcessResult::Skipped { reason } => JsonProcessResult {
                post_id,
                stance: "skipped".to_string(),
                raw_path: None,
                thesis_slug: None,
                x_post_id: None,
                nostr_event_id: None,
                error: Some(reason),
            },
            ProcessResult::Failed { error } => JsonProcessResult {
                post_id,
                stance: "failed".to_string(),
                raw_path: None,
                thesis_slug: None,
                x_post_id: None,
                nostr_event_id: None,
                error: Some(error),
            },
        })
        .collect()
}
