//! Port definitions (traits) for external dependencies.

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

use crate::model::{
    AccountState, AgentReturn, PostContext, ProcessedPostRecord, RenderedPost, SourcePost,
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
}

/// Error type for harness operations.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("process exited with status {status}: {stderr}")]
    Exit { status: String, stderr: String },
    #[error("process timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("validation error: {0}")]
    Validation(String),
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
    /// Platform-specific post/event ID.
    pub id: String,
    /// URL to the published content, if available.
    pub url: Option<String>,
}

/// Port for publishing rendered commentary.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Publish a rendered post, returns the published ID.
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
    async fn get_processed(&self, post_id: &str)
    -> Result<Option<ProcessedPostRecord>, StateError>;
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
