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
use tokio::time::{MissedTickBehavior, interval};

use crate::args::RunArgs;
use crate::commands::common::{
    build_harness, build_post_source, build_publishers, load_configured_lens,
};
use crate::config::AppConfig;

const RUN_ONCE_ALL_FAILED_EXIT_CODE: i32 = 2;

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
        let report = run_loop.poll_once_report().await?;
        log_results(&report.results);
        if let Some(exit_code) = run_once_failure_exit_code(
            &report.results,
            report.account_errors.len(),
            config.watch.accounts.len(),
        ) {
            if report.results.is_empty() {
                tracing::error!(
                    accounts = report.account_errors.len(),
                    exit_code,
                    "all {} accounts failed before processing posts; exiting 2",
                    report.account_errors.len()
                );
            } else {
                tracing::error!(
                    posts = report.results.len(),
                    exit_code,
                    "all {} posts failed; exiting 2",
                    report.results.len()
                );
            }
            std::process::exit(exit_code);
        }
        return Ok(());
    }

    let poll_interval = Duration::from_secs(config.watch.poll_interval_secs);
    let mut ticker = interval(poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let shutdown = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        tracing::info!("Shutdown signal received");
    };
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;

            _ = &mut shutdown => {
                tracing::info!("Shutting down gracefully");
                break;
            }
            _ = ticker.tick() => {
                match run_loop.poll_once().await {
                    Ok(results) => log_results(&results),
                    Err(error) => tracing::error!(error = %error, "Poll cycle failed"),
                }
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

fn run_once_failure_exit_code(
    results: &[(String, ProcessResult)],
    account_error_count: usize,
    configured_account_count: usize,
) -> Option<i32> {
    let all_posts_failed = !results.is_empty()
        && results
            .iter()
            .all(|(_, result)| matches!(result, ProcessResult::Failed { .. }));
    let all_accounts_failed = results.is_empty()
        && configured_account_count > 0
        && account_error_count == configured_account_count;

    (all_posts_failed || all_accounts_failed).then_some(RUN_ONCE_ALL_FAILED_EXIT_CODE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: ProcessResult) -> (String, ProcessResult) {
        ("post-1".to_string(), status)
    }

    #[test]
    fn run_once_empty_results_exit_success() {
        assert_eq!(run_once_failure_exit_code(&[], 0, 0), None);
    }

    #[test]
    fn run_once_empty_successful_account_results_exit_success() {
        assert_eq!(run_once_failure_exit_code(&[], 0, 1), None);
    }

    #[test]
    fn run_once_mixed_results_exit_success() {
        let results = vec![
            result(ProcessResult::Failed {
                error: "harness failed".to_string(),
            }),
            result(ProcessResult::Skipped {
                reason: "already processed".to_string(),
            }),
        ];

        assert_eq!(run_once_failure_exit_code(&results, 0, 1), None);
    }

    #[test]
    fn run_once_all_failed_results_exit_2() {
        let results = vec![
            result(ProcessResult::Failed {
                error: "harness failed".to_string(),
            }),
            result(ProcessResult::Failed {
                error: "publish failed".to_string(),
            }),
        ];

        assert_eq!(
            run_once_failure_exit_code(&results, 0, 1),
            Some(RUN_ONCE_ALL_FAILED_EXIT_CODE)
        );
    }

    #[test]
    fn run_once_all_accounts_failed_results_exit_2() {
        assert_eq!(
            run_once_failure_exit_code(&[], 2, 2),
            Some(RUN_ONCE_ALL_FAILED_EXIT_CODE)
        );
    }
}
