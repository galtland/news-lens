//! Serial run loop use case.

use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant, sleep};

use crate::{
    model::{
        AccountState, AgentReturn, Lens, PostContext, ProcessResult, ProcessedPostRecord,
        RenderedPost, SourcePost, Stance,
    },
    ports::{
        Clock, Harness, HarnessError, PostSource, Publisher, StateError, StateStore, SystemClock,
    },
};

/// Configuration for the run loop.
#[derive(Debug, Clone)]
pub struct RunLoopConfig {
    pub accounts: Vec<String>,
    pub include_replies: bool,
    pub include_reposts: bool,
    pub ignore_patterns: Vec<String>,
    pub dry_run: bool,
    pub wiki_path: std::path::PathBuf,
    pub lens: Lens,
    pub rate_limit_per_minute: Option<u32>,
    pub rate_limit_per_hour: Option<u32>,
}

impl Default for RunLoopConfig {
    fn default() -> Self {
        Self {
            accounts: vec![],
            include_replies: false,
            include_reposts: false,
            ignore_patterns: vec![],
            dry_run: true,
            wiki_path: std::path::PathBuf::new(),
            lens: Lens {
                id: String::new(),
                voice: None,
                register: None,
                path: std::path::PathBuf::new(),
                content: String::new(),
            },
            rate_limit_per_minute: None,
            rate_limit_per_hour: None,
        }
    }
}

/// Run loop orchestrator.
#[derive(Clone)]
pub struct RunLoop<S, H, X, N, St, Cl = SystemClock>
where
    S: PostSource + ?Sized,
    H: Harness + ?Sized,
    X: Publisher + ?Sized,
    N: Publisher + ?Sized,
    St: StateStore + ?Sized,
    Cl: Clock + ?Sized,
{
    post_source: Arc<S>,
    harness: Arc<H>,
    x_publisher: Arc<X>,
    nostr_publisher: Arc<N>,
    state_store: Arc<St>,
    clock: Arc<Cl>,
    config: RunLoopConfig,
    ignore_patterns: Vec<Regex>,
    rate_limiter: Arc<RateLimiter>,
}

impl<S, H, X, N, St, Cl> RunLoop<S, H, X, N, St, Cl>
where
    S: PostSource + ?Sized,
    H: Harness + ?Sized,
    X: Publisher + ?Sized,
    N: Publisher + ?Sized,
    St: StateStore + ?Sized,
    Cl: Clock + ?Sized,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        post_source: Arc<S>,
        harness: Arc<H>,
        x_publisher: Arc<X>,
        nostr_publisher: Arc<N>,
        state_store: Arc<St>,
        clock: Arc<Cl>,
        config: RunLoopConfig,
    ) -> Self {
        let ignore_patterns = compile_ignore_patterns(&config.ignore_patterns);
        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit_per_minute,
            config.rate_limit_per_hour,
        ));

        Self {
            post_source,
            harness,
            x_publisher,
            nostr_publisher,
            state_store,
            clock,
            config,
            ignore_patterns,
            rate_limiter,
        }
    }

    /// Run a single poll cycle for all configured accounts.
    pub async fn poll_once(&self) -> Result<Vec<(String, ProcessResult)>, RunLoopError> {
        let mut results = Vec::new();

        for account in &self.config.accounts {
            match self.poll_account(account).await {
                Ok(account_results) => results.extend(account_results),
                Err(error) => {
                    tracing::error!(account = %account, error = %error, "Failed to poll account");
                }
            }
        }

        Ok(results)
    }

    /// Process posts already supplied by a caller, used by `process --jsonl`.
    pub async fn process_posts(
        &self,
        posts: Vec<SourcePost>,
    ) -> Result<Vec<(String, ProcessResult)>, RunLoopError> {
        let mut results = Vec::new();
        for post in self.filter_posts(posts) {
            self.rate_limiter.acquire().await;
            let post_id = post.id.clone();
            let result = self.process_post(&post).await;
            results.push((post_id, result));
        }
        Ok(results)
    }

    async fn poll_account(
        &self,
        account: &str,
    ) -> Result<Vec<(String, ProcessResult)>, RunLoopError> {
        let account_state = self
            .state_store
            .get_account_state(account)
            .await
            .map_err(|error| RunLoopError::State(error.to_string()))?;

        let since_id = account_state
            .as_ref()
            .and_then(|state| state.since_id.as_deref());

        tracing::info!(account = %account, since_id = ?since_id, "Fetching posts");

        let posts = self
            .post_source
            .fetch_posts(account, since_id)
            .await
            .map_err(|error| RunLoopError::PostSource(error.to_string()))?;

        let filtered_posts = self.filter_posts(posts);
        let mut results = Vec::new();
        let mut last_id = since_id.map(String::from);

        for post in filtered_posts {
            last_id = Some(post.id.clone());
            self.rate_limiter.acquire().await;
            let post_id = post.id.clone();
            let result = self.process_post(&post).await;
            results.push((post_id, result));
        }

        if let Some(last_id) = last_id {
            let new_state = AccountState {
                account: account.to_string(),
                since_id: Some(last_id),
                updated_at: self.clock.now(),
            };
            self.state_store
                .set_account_state(&new_state)
                .await
                .map_err(|error| RunLoopError::State(error.to_string()))?;
        }

        Ok(results)
    }

    fn filter_posts(&self, posts: Vec<SourcePost>) -> Vec<SourcePost> {
        posts
            .into_iter()
            .filter(|post| {
                if !self.config.include_replies && post.is_reply {
                    return false;
                }
                if !self.config.include_reposts && post.is_repost {
                    return false;
                }
                if self
                    .ignore_patterns
                    .iter()
                    .any(|pattern| pattern.is_match(&post.text))
                {
                    return false;
                }
                true
            })
            .collect()
    }

    async fn process_post(&self, post: &SourcePost) -> ProcessResult {
        match self.state_store.is_processed(&post.id).await {
            Ok(true) => {
                return ProcessResult::Skipped {
                    reason: "Already processed".to_string(),
                };
            }
            Err(error) => {
                tracing::warn!(error = %error, "Failed to check processed state, continuing");
            }
            Ok(false) => {}
        }

        let ctx = PostContext {
            post: post.clone(),
            wiki_path: self.config.wiki_path.clone(),
            lens: self.config.lens.clone(),
            candidate_slug: candidate_slug(post),
        };

        let agent_return = match self.harness.process_post(ctx).await {
            Ok(agent_return) => agent_return,
            Err(error) => {
                let message = format!("Harness failed: {}", error);
                if let Err(state_error) = self.record_failed(post).await {
                    tracing::error!(error = %state_error, "Failed to record harness failure");
                }
                return ProcessResult::Failed { error: message };
            }
        };

        let mut x_post_id = None;
        let mut nostr_event_id = None;

        if agent_return.stance != Stance::Decline && agent_return.stance != Stance::Failed {
            if self.config.dry_run {
                tracing::info!(
                    post_id = %post.id,
                    one_liner = ?agent_return.one_liner,
                    "[DRY RUN] Would publish commentary"
                );
            } else {
                let rendered = render_reply(post, &agent_return);

                if self.x_publisher.is_enabled() {
                    match self.x_publisher.publish(&rendered).await {
                        Ok(result) => x_post_id = Some(result.id),
                        Err(error) => {
                            tracing::error!(error = %error, "Failed to publish to X");
                        }
                    }
                }

                if self.nostr_publisher.is_enabled() {
                    match self.nostr_publisher.publish(&rendered).await {
                        Ok(result) => nostr_event_id = Some(result.id),
                        Err(error) => {
                            tracing::error!(error = %error, "Failed to publish to Nostr");
                        }
                    }
                }
            }
        }

        let record = ProcessedPostRecord {
            post_id: post.id.clone(),
            lens_id: self.config.lens.id.clone(),
            processed_at: self.clock.now(),
            stance: agent_return.stance,
            raw_path: Some(agent_return.raw_path.clone()),
            thesis_slug: agent_return.thesis_slug.clone(),
            x_post_id: x_post_id.clone(),
            nostr_event_id: nostr_event_id.clone(),
        };

        if let Err(error) = self.state_store.record_processed(&record).await {
            tracing::error!(error = %error, "Failed to record processed state");
        }

        ProcessResult::Processed {
            source_post: Box::new(post.clone()),
            agent_return,
            x_post_id,
            nostr_event_id,
        }
    }

    async fn record_failed(&self, post: &SourcePost) -> Result<(), StateError> {
        let record = ProcessedPostRecord {
            post_id: post.id.clone(),
            lens_id: self.config.lens.id.clone(),
            processed_at: self.clock.now(),
            stance: Stance::Failed,
            raw_path: None,
            thesis_slug: None,
            x_post_id: None,
            nostr_event_id: None,
        };
        self.state_store.record_processed(&record).await
    }
}

/// Errors from the run loop.
#[derive(Debug, thiserror::Error)]
pub enum RunLoopError {
    #[error("Post source error: {0}")]
    PostSource(String),
    #[error("State error: {0}")]
    State(String),
}

#[derive(Debug)]
struct RateLimiter {
    per_minute: Option<u32>,
    per_hour: Option<u32>,
    state: Mutex<RateLimiterState>,
}

#[derive(Debug)]
struct RateLimiterState {
    minute_window_start: Instant,
    hour_window_start: Instant,
    minute_count: u32,
    hour_count: u32,
}

impl RateLimiter {
    fn new(per_minute: Option<u32>, per_hour: Option<u32>) -> Self {
        let now = Instant::now();
        Self {
            per_minute,
            per_hour,
            state: Mutex::new(RateLimiterState {
                minute_window_start: now,
                hour_window_start: now,
                minute_count: 0,
                hour_count: 0,
            }),
        }
    }

    async fn acquire(&self) {
        if self.per_minute.is_none() && self.per_hour.is_none() {
            return;
        }

        loop {
            let mut state = self.state.lock().await;
            let now = Instant::now();

            if now.duration_since(state.minute_window_start) >= Duration::from_secs(60) {
                state.minute_window_start = now;
                state.minute_count = 0;
            }

            if now.duration_since(state.hour_window_start) >= Duration::from_secs(3600) {
                state.hour_window_start = now;
                state.hour_count = 0;
            }

            let mut wait_for = Duration::from_secs(0);
            if let Some(limit) = self.per_minute {
                if state.minute_count >= limit {
                    let elapsed = now.duration_since(state.minute_window_start);
                    wait_for = wait_for.max(Duration::from_secs(60).saturating_sub(elapsed));
                }
            }

            if let Some(limit) = self.per_hour {
                if state.hour_count >= limit {
                    let elapsed = now.duration_since(state.hour_window_start);
                    wait_for = wait_for.max(Duration::from_secs(3600).saturating_sub(elapsed));
                }
            }

            if wait_for.is_zero() {
                if self.per_minute.is_some() {
                    state.minute_count = state.minute_count.saturating_add(1);
                }
                if self.per_hour.is_some() {
                    state.hour_count = state.hour_count.saturating_add(1);
                }
                return;
            }

            drop(state);
            sleep(wait_for).await;
        }
    }
}

fn compile_ignore_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| match Regex::new(pattern) {
            Ok(regex) => Some(regex),
            Err(error) => {
                tracing::warn!(pattern = %pattern, error = %error, "Invalid ignore pattern");
                None
            }
        })
        .collect()
}

fn render_reply(post: &SourcePost, agent_return: &AgentReturn) -> RenderedPost {
    RenderedPost {
        text: agent_return.one_liner.clone().unwrap_or_default(),
        source_post_id: post.id.clone(),
        source_post_url: post.url.clone(),
    }
}

pub fn candidate_slug(post: &SourcePost) -> String {
    let date = post
        .created_at
        .date()
        .to_string()
        .replace('[', "")
        .replace(']', "");
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in post.text.chars().flat_map(char::to_lowercase).take(120) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("{}-{}", date, post.id)
    } else {
        format!("{}-{}", date, slug)
    }
}

#[async_trait]
impl<H: Harness + ?Sized> Harness for &H {
    async fn process_post(&self, ctx: PostContext) -> Result<AgentReturn, HarnessError> {
        (*self).process_post(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{PostSourceError, PublishError, PublishResult};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use time::OffsetDateTime;

    struct FakePostSource {
        posts: Vec<SourcePost>,
    }

    #[async_trait]
    impl PostSource for FakePostSource {
        async fn fetch_posts(
            &self,
            _account: &str,
            _since_id: Option<&str>,
        ) -> Result<Vec<SourcePost>, PostSourceError> {
            Ok(self.posts.clone())
        }
    }

    struct FakeHarness {
        result: AgentReturn,
    }

    #[async_trait]
    impl Harness for FakeHarness {
        async fn process_post(&self, _ctx: PostContext) -> Result<AgentReturn, HarnessError> {
            Ok(self.result.clone())
        }
    }

    struct FakePublisher {
        enabled: bool,
        published: StdMutex<Vec<RenderedPost>>,
    }

    #[async_trait]
    impl Publisher for FakePublisher {
        async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
            self.published.lock().unwrap().push(post.clone());
            Ok(PublishResult {
                id: "published-id".to_string(),
                url: None,
            })
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn platform(&self) -> &'static str {
            "fake"
        }
    }

    struct FakeStateStore {
        accounts: StdMutex<HashMap<String, AccountState>>,
        processed: StdMutex<HashMap<String, ProcessedPostRecord>>,
    }

    #[async_trait]
    impl StateStore for FakeStateStore {
        async fn get_account_state(
            &self,
            account: &str,
        ) -> Result<Option<AccountState>, StateError> {
            Ok(self.accounts.lock().unwrap().get(account).cloned())
        }

        async fn set_account_state(&self, state: &AccountState) -> Result<(), StateError> {
            self.accounts
                .lock()
                .unwrap()
                .insert(state.account.clone(), state.clone());
            Ok(())
        }

        async fn is_processed(&self, post_id: &str) -> Result<bool, StateError> {
            Ok(self.processed.lock().unwrap().contains_key(post_id))
        }

        async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError> {
            self.processed
                .lock()
                .unwrap()
                .insert(record.post_id.clone(), record.clone());
            Ok(())
        }

        async fn get_processed(
            &self,
            post_id: &str,
        ) -> Result<Option<ProcessedPostRecord>, StateError> {
            Ok(self.processed.lock().unwrap().get(post_id).cloned())
        }
    }

    #[derive(Default)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH
        }
    }

    fn sample_post(id: &str) -> SourcePost {
        SourcePost {
            id: id.to_string(),
            text: "Test news item".to_string(),
            author: "tester".to_string(),
            url: "https://example.com/post".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            is_repost: false,
            is_reply: false,
            reply_to_id: None,
        }
    }

    fn sample_lens() -> Lens {
        Lens {
            id: "test-lens".to_string(),
            voice: None,
            register: None,
            path: "lens.md".into(),
            content: "Lens body".to_string(),
        }
    }

    fn sample_agent_return() -> AgentReturn {
        AgentReturn {
            stance: Stance::Critique,
            raw_path: "raw/news/item.md".to_string(),
            raw_slug: Some("item".to_string()),
            thesis_path: Some("theses/item.md".to_string()),
            thesis_slug: Some("item".to_string()),
            one_liner: Some("One line.".to_string()),
        }
    }

    #[tokio::test]
    async fn poll_once_processes_posts_serially_and_records_state() {
        let state = Arc::new(FakeStateStore {
            accounts: StdMutex::new(HashMap::new()),
            processed: StdMutex::new(HashMap::new()),
        });
        let x = Arc::new(FakePublisher {
            enabled: false,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1")],
            }),
            Arc::new(FakeHarness {
                result: sample_agent_return(),
            }),
            x,
            nostr,
            state.clone(),
            Arc::new(FixedClock),
            RunLoopConfig {
                accounts: vec!["tester".to_string()],
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop.poll_once().await.expect("poll");

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].1, ProcessResult::Processed { .. }));
        assert!(state.get_processed("1").await.unwrap().is_some());
    }

    #[test]
    fn candidate_slug_uses_date_and_text() {
        assert_eq!(
            candidate_slug(&sample_post("1")),
            "1970-01-01-test-news-item"
        );
    }
}
