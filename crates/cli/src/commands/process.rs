//! Process command.

use anyhow::{Context, Result, bail};
use news_lens_adapters::{
    jsonl::JsonlPostSource,
    nostr::NostrPublisher,
    state::{InMemoryStateStore, SqliteStateStore},
    x::StubXPublisher,
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
use crate::commands::common::{build_harness, load_configured_lens};
use crate::config::AppConfig;

pub async fn execute(args: ProcessArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;
    let lens = load_configured_lens(&config)?;
    let harness = build_harness(&config);

    if args.require_approval {
        tracing::warn!(
            "--require-approval is accepted for consistency but process does not publish"
        );
    }

    if let Some(post_arg) = args.post {
        let post = synthetic_post(post_arg, args.text)?;
        let ctx = news_lens_domain::PostContext {
            post,
            wiki_path: config.wiki.path.clone(),
            lens,
            candidate_slug: String::new(),
        };
        let ctx = news_lens_domain::PostContext {
            candidate_slug: candidate_slug(&ctx.post),
            ..ctx
        };
        let output = harness
            .process_post(ctx)
            .await
            .context("Harness processing failed")?;
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if let Some(path) = args.jsonl {
        let source = JsonlPostSource::new(vec![path]);
        let posts = source
            .fetch_posts("*", None)
            .await
            .context("Failed to load JSONL posts")?;
        let state_store: Arc<dyn StateStore> = if args.dry_run {
            Arc::new(InMemoryStateStore::new())
        } else {
            Arc::new(
                SqliteStateStore::new(&config.general.state_db_path)
                    .await
                    .context("Failed to initialize SQLite state store")?,
            )
        };
        let disabled_x = Arc::new(StubXPublisher::new(false));
        let disabled_nostr = Arc::new(NostrPublisher::disabled());
        let run_loop = RunLoop::new(
            Arc::new(source),
            Arc::new(harness),
            disabled_x,
            disabled_nostr,
            state_store,
            Arc::new(SystemClock),
            RunLoopConfig {
                accounts: vec![],
                include_replies: config.watch.include_replies,
                include_reposts: config.watch.include_reposts,
                ignore_patterns: config.watch.ignore_patterns.clone(),
                dry_run: true,
                wiki_path: config.wiki.path.clone(),
                lens,
                rate_limit_per_minute: None,
                rate_limit_per_hour: None,
            },
        );
        let results = run_loop.process_posts(posts).await?;
        for result in results_to_json(results) {
            println!("{}", serde_json::to_string(&result)?);
        }
        return Ok(());
    }

    bail!("Expected --post or --jsonl")
}

fn synthetic_post(post_arg: Option<String>, text_arg: Option<String>) -> Result<SourcePost> {
    let text = match (post_arg, text_arg) {
        (_, Some(text)) => text,
        (Some(value), None) => value,
        (None, None) => bail!("--post requires a value or --text"),
    };

    if text.trim().is_empty() {
        bail!("post text is empty");
    }

    Ok(SourcePost {
        id: "cli-input".to_string(),
        text,
        author: "cli".to_string(),
        url: String::new(),
        created_at: OffsetDateTime::now_utc(),
        is_repost: false,
        is_reply: false,
        reply_to_id: None,
    })
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
