//! X (Twitter) API adapters

mod read;
mod write;

pub use read::XPostSource;
pub use write::XPublisher;
pub(crate) use write::format_new_post_text;

use async_trait::async_trait;
use news_lens_domain::{
    PostSource, PostSourceError, PublishError, PublishResult, Publisher, RenderedPost, SourcePost,
};

/// Stub post source for testing
pub struct StubPostSource {
    posts: Vec<SourcePost>,
}

impl StubPostSource {
    /// Create an empty stub
    pub fn empty() -> Self {
        Self { posts: vec![] }
    }

    /// Create a stub with predefined posts
    pub fn with_posts(posts: Vec<SourcePost>) -> Self {
        Self { posts }
    }
}

#[async_trait]
impl PostSource for StubPostSource {
    async fn fetch_posts(
        &self,
        _account: &str,
        _since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError> {
        Ok(self.posts.clone())
    }
}

/// Stub X publisher for testing
pub struct StubXPublisher {
    enabled: bool,
    published: std::sync::Mutex<Vec<RenderedPost>>,
}

impl StubXPublisher {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            published: std::sync::Mutex::new(vec![]),
        }
    }

    /// Get all posts that were published
    pub fn get_published(&self) -> Vec<RenderedPost> {
        self.published.lock().unwrap().clone()
    }
}

#[async_trait]
impl Publisher for StubXPublisher {
    async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
        if !self.enabled {
            return Err(PublishError::Api("Publisher disabled".to_string()));
        }

        self.published.lock().unwrap().push(post.clone());

        Ok(PublishResult {
            id: Some(format!("stub_{}", post.source_post_id)),
            url: Some(format!(
                "https://x.com/stub/status/stub_{}",
                post.source_post_id
            )),
        })
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn platform(&self) -> &'static str {
        "x"
    }
}
