//! Port definitions (traits) for external dependencies.

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

use crate::model::{
    AccountState, AgentReturn, PostContext, ProcessedPostRecord, RawAgentReturn, RenderedPost,
    SourcePost,
};

/// Error type for post source operations.
#[derive(Debug, Error)]
pub enum PostSourceError {
    #[error("API error: {0}")]
    Api(String),
    #[error("Rate limited, retry after: {0:?}")]
    RateLimited(Option<std::time::Duration>),
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("Network error: {0}")]
    Network(String),
}

/// Port for fetching posts from a source platform.
#[async_trait]
pub trait PostSource: Send + Sync {
    /// Fetch posts for an account since the given ID.
    async fn fetch_posts(
        &self,
        account: &str,
        since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError>;

    /// Fetch a specific source post by platform ID when the adapter supports it.
    ///
    /// The default implementation fetches all locally available posts by passing
    /// `"*"` to `fetch_posts`. Adapters using this default must treat that
    /// account sentinel as "all accounts"; remote adapters should override this.
    async fn fetch_post_by_id(&self, post_id: &str) -> Result<Option<SourcePost>, PostSourceError> {
        Ok(self
            .fetch_posts("*", None)
            .await?
            .into_iter()
            .find(|post| post.id == post_id))
    }
}

/// Error type for harness operations.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("IO error: {0}")]
    Io(String),
    #[error(
        "process exited with status {status}: {stderr}; stdout tail: {stdout_tail}; parse error: {parse_error:?}"
    )]
    Exit {
        status: String,
        stderr: String,
        stdout_tail: String,
        parse_error: Option<String>,
        raw: Option<Box<RawAgentReturn>>,
    },
    #[error("process timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("validation error: {message}")]
    Validation {
        message: String,
        raw: Box<RawAgentReturn>,
    },
}

/// Port for the subprocess harness.
#[async_trait]
pub trait Harness: Send + Sync {
    async fn process_post(&self, ctx: PostContext) -> Result<AgentReturn, HarnessError>;
}

/// Error type for publisher operations.
#[derive(Debug, Error)]
pub enum PublishError {
    #[error("API error: {0}")]
    Api(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("Content too long: {len} > {max}")]
    ContentTooLong { len: usize, max: usize },
}

/// Result of a successful publish operation.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// Platform-specific post/event ID, if the content was actually published.
    pub id: Option<String>,
    /// URL to the published content, if available.
    pub url: Option<String>,
}

/// Port for publishing rendered commentary.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Publish a rendered post.
    async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError>;

    /// Check if this publisher is enabled.
    fn is_enabled(&self) -> bool;

    /// Get the platform name (e.g. "x", "nostr").
    fn platform(&self) -> &'static str;
}

/// Error type for state store operations.
#[derive(Debug, Error)]
pub enum StateError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Port for persisting application state.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Get account state (since_id, etc.).
    async fn get_account_state(&self, account: &str) -> Result<Option<AccountState>, StateError>;

    /// Update account state.
    async fn set_account_state(&self, state: &AccountState) -> Result<(), StateError>;

    /// Check if a post has already been processed for the active lens.
    async fn is_processed(&self, post_id: &str, lens_id: &str) -> Result<bool, StateError>;

    /// Record one processed post.
    async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError>;

    /// Get the processed record for a post.
    async fn get_processed(
        &self,
        post_id: &str,
        lens_id: &str,
    ) -> Result<Option<ProcessedPostRecord>, StateError>;
}

/// Port for time/clock operations (enables deterministic testing).
pub trait Clock: Send + Sync {
    /// Get the current time.
    fn now(&self) -> OffsetDateTime;
}

/// Real clock implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}
