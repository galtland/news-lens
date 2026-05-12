//! Serial run loop use case.

use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;

use crate::{
    compare_post_ids,
    model::{
        AccountState, AgentReturn, Lens, PostContext, ProcessResult, ProcessedPostRecord,
        RawAgentReturn, RenderedPost, SourcePost, Stance,
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

        Self {
            post_source,
            harness,
            x_publisher,
            nostr_publisher,
            state_store,
            clock,
            config,
            ignore_patterns,
        }
    }

    /// Run a single poll cycle for all configured accounts.
    pub async fn poll_once(&self) -> Result<Vec<(String, ProcessResult)>, RunLoopError> {
        let mut results = Vec::new();

        for account in &self.config.accounts {
            match self.poll_account(account).await {
                Ok(account_results) => results.extend(account_results),
                Err(error) => {
                    tracing::error!(
                        account = %account,
                        error = %error,
                        "Failed to poll account; continuing with remaining accounts"
                    );
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
            let post_id = post.id.clone();
            let result = self.process_post(&post).await?;
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

        let last_fetched_id = posts
            .iter()
            .map(|post| post.id.as_str())
            .max_by(|a, b| compare_post_ids(a, b))
            .map(str::to_string);
        let filtered_posts = self.filter_posts(posts);
        let mut results = Vec::new();

        for post in filtered_posts {
            let post_id = post.id.clone();
            let result = self.process_post(&post).await?;
            results.push((post_id, result));
        }

        if let Some(last_id) = last_fetched_id.or_else(|| since_id.map(String::from)) {
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

    async fn process_post(&self, post: &SourcePost) -> Result<ProcessResult, RunLoopError> {
        match self.state_store.is_processed(&post.id).await {
            Ok(true) => {
                return Ok(ProcessResult::Skipped {
                    reason: "Already processed".to_string(),
                });
            }
            Err(error) => return Err(state_error(error)),
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
                let raw = match &error {
                    HarnessError::Validation { raw, .. } => Some(raw.as_ref()),
                    _ => None,
                };
                let message = format!("Harness failed: {}", error);
                self.record_failed(post, raw).await.map_err(state_error)?;
                return Ok(ProcessResult::Failed { error: message });
            }
        };

        if agent_return.stance == Stance::Failed {
            self.record_agent_return(post, &agent_return, None, None)
                .await
                .map_err(state_error)?;
            return Ok(ProcessResult::Failed {
                error: "Agent returned failed stance".to_string(),
            });
        }

        let mut x_post_id = None;
        let mut nostr_event_id = None;
        let mut publish_errors = Vec::new();
        let publishers_enabled = self.x_publisher.is_enabled() || self.nostr_publisher.is_enabled();
        let should_publish =
            agent_return.stance != Stance::Decline && !self.config.dry_run && publishers_enabled;

        if agent_return.stance != Stance::Decline {
            if self.config.dry_run && publishers_enabled {
                tracing::info!(
                    post_id = %post.id,
                    one_liner = ?agent_return.one_liner,
                    "[DRY RUN] Would publish commentary"
                );
            } else if should_publish {
                // Record before publishing so a successful outbox/publish action is not retried
                // if the later platform-ID update fails.
                self.record_agent_return(post, &agent_return, None, None)
                    .await
                    .map_err(state_error)?;

                let rendered = render_reply(post, &agent_return);

                if self.x_publisher.is_enabled() {
                    match self.x_publisher.publish(&rendered).await {
                        Ok(result) => x_post_id = result.id,
                        Err(error) => {
                            tracing::error!(error = %error, "Failed to publish to X");
                            publish_errors.push(format!("X publish failed: {}", error));
                        }
                    }
                }

                if self.nostr_publisher.is_enabled() {
                    let rendered = render_nostr_note(post, &agent_return);
                    match self.nostr_publisher.publish(&rendered).await {
                        Ok(result) => nostr_event_id = result.id,
                        Err(error) => {
                            tracing::error!(error = %error, "Failed to publish to Nostr");
                            publish_errors.push(format!("Nostr publish failed: {}", error));
                        }
                    }
                }
            }
        }

        if should_publish || !publish_errors.is_empty() {
            if let Err(error) = self
                .record_agent_return(
                    post,
                    &agent_return,
                    x_post_id.clone(),
                    nostr_event_id.clone(),
                )
                .await
            {
                tracing::error!(
                    post_id = %post.id,
                    x_post_id = ?x_post_id,
                    nostr_event_id = ?nostr_event_id,
                    raw_path = %agent_return.raw_path,
                    thesis_slug = ?agent_return.thesis_slug,
                    error = %error,
                    "Failed to record publish result after publishing"
                );
                return Err(state_error(error));
            }
        } else {
            self.record_agent_return(post, &agent_return, None, None)
                .await
                .map_err(state_error)?;
        }

        if !publish_errors.is_empty() {
            return Ok(ProcessResult::Failed {
                error: publish_errors.join("; "),
            });
        }

        Ok(ProcessResult::Processed {
            source_post: Box::new(post.clone()),
            agent_return,
            x_post_id,
            nostr_event_id,
        })
    }

    async fn record_failed(
        &self,
        post: &SourcePost,
        raw: Option<&RawAgentReturn>,
    ) -> Result<(), StateError> {
        let record = ProcessedPostRecord {
            post_id: post.id.clone(),
            lens_id: self.config.lens.id.clone(),
            processed_at: self.clock.now(),
            stance: Stance::Failed,
            raw_path: raw.and_then(|raw| raw.raw_path.clone()),
            thesis_slug: raw.and_then(|raw| raw.thesis_slug.clone()),
            x_post_id: None,
            nostr_event_id: None,
        };
        self.state_store.record_processed(&record).await
    }

    async fn record_agent_return(
        &self,
        post: &SourcePost,
        agent_return: &AgentReturn,
        x_post_id: Option<String>,
        nostr_event_id: Option<String>,
    ) -> Result<(), StateError> {
        let record = ProcessedPostRecord {
            post_id: post.id.clone(),
            lens_id: self.config.lens.id.clone(),
            processed_at: self.clock.now(),
            stance: agent_return.stance,
            raw_path: Some(agent_return.raw_path.clone()),
            thesis_slug: agent_return.thesis_slug.clone(),
            x_post_id,
            nostr_event_id,
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

fn state_error(error: StateError) -> RunLoopError {
    RunLoopError::State(error.to_string())
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

fn render_nostr_note(post: &SourcePost, agent_return: &AgentReturn) -> RenderedPost {
    let mut rendered = render_reply(post, agent_return);
    let source_url = rendered.source_post_url.trim();

    if source_url.is_empty() {
        return rendered;
    }

    rendered.text = if rendered.text.trim().is_empty() {
        source_url.to_string()
    } else {
        format!("{}\n\n{}", rendered.text.trim_end(), source_url)
    };
    rendered
}

pub fn candidate_slug(post: &SourcePost) -> String {
    let date = post.created_at.date().to_string();
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

    struct PerAccountPostSource;

    #[async_trait]
    impl PostSource for PerAccountPostSource {
        async fn fetch_posts(
            &self,
            account: &str,
            _since_id: Option<&str>,
        ) -> Result<Vec<SourcePost>, PostSourceError> {
            if account == "bad" {
                Err(PostSourceError::Api("temporary failure".to_string()))
            } else {
                Ok(vec![sample_post("1")])
            }
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

    struct ValidationFailingHarness;

    #[async_trait]
    impl Harness for ValidationFailingHarness {
        async fn process_post(&self, _ctx: PostContext) -> Result<AgentReturn, HarnessError> {
            Err(HarnessError::Validation {
                message: "missing field: thesis_path".to_string(),
                raw: Box::new(RawAgentReturn {
                    stance: Some("critique".to_string()),
                    raw_path: Some("raw/news/partial.md".to_string()),
                    raw_slug: Some("partial".to_string()),
                    thesis_path: None,
                    thesis_slug: Some("partial-thesis".to_string()),
                    one_liner: Some("Partial line".to_string()),
                }),
            })
        }
    }

    struct FakePublisher {
        enabled: bool,
        fail_message: Option<String>,
        published: StdMutex<Vec<RenderedPost>>,
    }

    #[async_trait]
    impl Publisher for FakePublisher {
        async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
            if let Some(message) = &self.fail_message {
                return Err(PublishError::Api(message.clone()));
            }
            self.published.lock().unwrap().push(post.clone());
            Ok(PublishResult {
                id: Some("published-id".to_string()),
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
        fail_is_processed: bool,
        record_calls: StdMutex<usize>,
        fail_record_call: Option<usize>,
        record_failures_remaining: StdMutex<usize>,
    }

    impl FakeStateStore {
        fn new() -> Self {
            Self {
                accounts: StdMutex::new(HashMap::new()),
                processed: StdMutex::new(HashMap::new()),
                fail_is_processed: false,
                record_calls: StdMutex::new(0),
                fail_record_call: None,
                record_failures_remaining: StdMutex::new(0),
            }
        }

        fn with_is_processed_failure() -> Self {
            Self {
                fail_is_processed: true,
                ..Self::new()
            }
        }

        fn with_record_failures(count: usize) -> Self {
            Self {
                record_failures_remaining: StdMutex::new(count),
                ..Self::new()
            }
        }

        fn with_record_failure_on_call(call: usize) -> Self {
            Self {
                fail_record_call: Some(call),
                ..Self::new()
            }
        }
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
            if self.fail_is_processed {
                return Err(StateError::Database("is_processed failed".to_string()));
            }
            Ok(self.processed.lock().unwrap().contains_key(post_id))
        }

        async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError> {
            let mut calls = self.record_calls.lock().unwrap();
            *calls += 1;
            if self.fail_record_call == Some(*calls) {
                return Err(StateError::Database("record_processed failed".to_string()));
            }
            drop(calls);

            let mut failures = self.record_failures_remaining.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(StateError::Database("record_processed failed".to_string()));
            }
            drop(failures);

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

    fn sample_failed_agent_return() -> AgentReturn {
        AgentReturn {
            stance: Stance::Failed,
            raw_path: "raw/news/item.md".to_string(),
            raw_slug: Some("item".to_string()),
            thesis_path: None,
            thesis_slug: None,
            one_liner: None,
        }
    }

    #[tokio::test]
    async fn poll_once_processes_posts_serially_and_records_state() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
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

    #[tokio::test]
    async fn poll_once_advances_cursor_past_filtered_posts() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let mut filtered_reply = sample_post("2");
        filtered_reply.is_reply = true;
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1"), filtered_reply],
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
        let account = state
            .get_account_state("tester")
            .await
            .expect("state")
            .expect("account state");
        assert_eq!(account.since_id.as_deref(), Some("2"));
    }

    #[tokio::test]
    async fn poll_once_continues_after_one_account_fails() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(PerAccountPostSource),
            Arc::new(FakeHarness {
                result: sample_agent_return(),
            }),
            x,
            nostr,
            state.clone(),
            Arc::new(FixedClock),
            RunLoopConfig {
                accounts: vec!["bad".to_string(), "good".to_string()],
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop.poll_once().await.expect("poll");

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].1, ProcessResult::Processed { .. }));
        assert!(state.get_processed("1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn publisher_failure_records_processed_stance_without_retrying() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: true,
            fail_message: Some("outbox unavailable".to_string()),
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
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
                accounts: vec![],
                dry_run: false,
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect("process");

        assert!(matches!(results[0].1, ProcessResult::Failed { .. }));
        let record = state
            .get_processed("1")
            .await
            .expect("state")
            .expect("recorded failure");
        assert_eq!(record.stance, Stance::Critique);
        assert_eq!(record.raw_path.as_deref(), Some("raw/news/item.md"));
        assert_eq!(record.thesis_slug.as_deref(), Some("item"));
    }

    #[tokio::test]
    async fn agent_failed_stance_records_state_and_returns_failed_result() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1")],
            }),
            Arc::new(FakeHarness {
                result: sample_failed_agent_return(),
            }),
            x,
            nostr,
            state.clone(),
            Arc::new(FixedClock),
            RunLoopConfig {
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect("process");

        assert!(
            matches!(&results[0].1, ProcessResult::Failed { error } if error.contains("failed stance"))
        );
        let record = state
            .get_processed("1")
            .await
            .expect("state")
            .expect("recorded failure");
        assert_eq!(record.stance, Stance::Failed);
        assert_eq!(record.raw_path.as_deref(), Some("raw/news/item.md"));
        assert!(record.thesis_slug.is_none());
    }

    #[tokio::test]
    async fn direct_nostr_publish_includes_source_url_in_note_text() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: true,
            fail_message: None,
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
            nostr.clone(),
            state,
            Arc::new(FixedClock),
            RunLoopConfig {
                dry_run: false,
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect("process");

        assert!(matches!(results[0].1, ProcessResult::Processed { .. }));
        let published = nostr.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].text, "One line.\n\nhttps://example.com/post");
    }

    #[tokio::test]
    async fn harness_validation_failure_records_partial_agent_fields() {
        let state = Arc::new(FakeStateStore::new());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1")],
            }),
            Arc::new(ValidationFailingHarness),
            x,
            nostr,
            state.clone(),
            Arc::new(FixedClock),
            RunLoopConfig {
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let results = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect("process");

        assert!(matches!(results[0].1, ProcessResult::Failed { .. }));
        let record = state
            .get_processed("1")
            .await
            .expect("state")
            .expect("recorded failure");
        assert_eq!(record.stance, Stance::Failed);
        assert_eq!(record.raw_path.as_deref(), Some("raw/news/partial.md"));
        assert_eq!(record.thesis_slug.as_deref(), Some("partial-thesis"));
    }

    #[tokio::test]
    async fn state_read_error_fails_without_processing() {
        let state = Arc::new(FakeStateStore::with_is_processed_failure());
        let x = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
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
            state,
            Arc::new(FixedClock),
            RunLoopConfig {
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let error = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect_err("state error");

        assert!(matches!(error, RunLoopError::State(message) if message.contains("is_processed")));
    }

    #[tokio::test]
    async fn state_write_error_fails_before_publishing() {
        let state = Arc::new(FakeStateStore::with_record_failures(1));
        let x = Arc::new(FakePublisher {
            enabled: true,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1")],
            }),
            Arc::new(FakeHarness {
                result: sample_agent_return(),
            }),
            x.clone(),
            nostr,
            state,
            Arc::new(FixedClock),
            RunLoopConfig {
                dry_run: false,
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let error = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect_err("state error");

        assert!(
            matches!(error, RunLoopError::State(message) if message.contains("record_processed"))
        );
        assert!(x.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn final_state_write_error_preserves_publish_result_in_error_log_branch() {
        let state = Arc::new(FakeStateStore::with_record_failure_on_call(2));
        let x = Arc::new(FakePublisher {
            enabled: true,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let nostr = Arc::new(FakePublisher {
            enabled: false,
            fail_message: None,
            published: StdMutex::new(vec![]),
        });
        let run_loop = RunLoop::new(
            Arc::new(FakePostSource {
                posts: vec![sample_post("1")],
            }),
            Arc::new(FakeHarness {
                result: sample_agent_return(),
            }),
            x.clone(),
            nostr,
            state,
            Arc::new(FixedClock),
            RunLoopConfig {
                dry_run: false,
                lens: sample_lens(),
                ..Default::default()
            },
        );

        let error = run_loop
            .process_posts(vec![sample_post("1")])
            .await
            .expect_err("state error");

        assert!(
            matches!(error, RunLoopError::State(message) if message.contains("record_processed"))
        );
        assert_eq!(x.published.lock().unwrap().len(), 1);
    }

    #[test]
    fn candidate_slug_uses_date_and_text() {
        assert_eq!(
            candidate_slug(&sample_post("1")),
            "1970-01-01-test-news-item"
        );
    }
}
