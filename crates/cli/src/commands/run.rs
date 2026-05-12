//! Run command.

use anyhow::{Context, Result};
use news_lens_adapters::state::SqliteStateStore;
use news_lens_domain::{
    ProcessResult, SystemClock,
    usecases::{RunLoop, RunLoopConfig},
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

use crate::args::RunArgs;
use crate::commands::common::{
    build_harness, build_post_source, build_publishers, load_configured_lens,
};
use crate::config::AppConfig;

pub async fn execute(args: RunArgs, config_path: Option<PathBuf>) -> Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;

    let mut dry_run = args.dry_run || config.general.dry_run;
    if args.require_approval && dry_run {
        tracing::info!("--require-approval overrides dry-run");
        dry_run = false;
    }

    tracing::info!(
        dry_run = dry_run,
        once = args.once,
        require_approval = args.require_approval,
        accounts = ?config.watch.accounts,
        "Starting news-lens run"
    );

    let lens = load_configured_lens(&config)?;
    let harness = Arc::new(build_harness(&config)?);
    let state_store = Arc::new(
        SqliteStateStore::new(&config.general.state_db_path)
            .await
            .context("Failed to initialize SQLite state store")?,
    );
    let post_source = build_post_source(&config)?;
    let (x_publisher, nostr_publisher) =
        build_publishers(&config, dry_run, args.require_approval, args.outbox).await?;
    let clock = Arc::new(SystemClock);

    let run_loop = RunLoop::new(
        post_source,
        harness,
        x_publisher,
        nostr_publisher,
        state_store,
        clock,
        RunLoopConfig {
            accounts: config.watch.accounts.clone(),
            include_replies: config.watch.include_replies,
            include_reposts: config.watch.include_reposts,
            ignore_patterns: config.watch.ignore_patterns.clone(),
            dry_run,
            wiki_path: config.wiki.path.clone(),
            lens,
        },
    );

    if args.once {
        let results = run_loop.poll_once().await?;
        log_results(&results);
        return Ok(());
    }

    let poll_interval = Duration::from_secs(config.watch.poll_interval_secs);
    let mut ticker = interval(poll_interval);

    let shutdown = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        tracing::info!("Shutdown signal received");
    };
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match run_loop.poll_once().await {
                    Ok(results) => log_results(&results),
                    Err(error) => tracing::error!(error = %error, "Poll cycle failed"),
                }
            }
            _ = &mut shutdown => {
                tracing::info!("Shutting down gracefully");
                break;
            }
        }
    }

    Ok(())
}

fn log_results(results: &[(String, ProcessResult)]) {
    if results.is_empty() {
        tracing::info!("Poll cycle complete; no posts processed");
        return;
    }

    tracing::info!(processed = results.len(), "Poll cycle complete");
    for (post_id, result) in results {
        match result {
            ProcessResult::Processed {
                agent_return,
                x_post_id,
                nostr_event_id,
                ..
            } => {
                tracing::info!(
                    post_id = %post_id,
                    stance = %agent_return.stance,
                    thesis_slug = ?agent_return.thesis_slug,
                    x_post_id = ?x_post_id,
                    nostr_event_id = ?nostr_event_id,
                    "Processed"
                );
            }
            ProcessResult::Skipped { reason } => {
                tracing::debug!(post_id = %post_id, reason = %reason, "Skipped");
            }
            ProcessResult::Failed { error } => {
                tracing::error!(post_id = %post_id, error = %error, "Failed");
            }
        }
    }
}
