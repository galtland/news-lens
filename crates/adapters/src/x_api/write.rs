//! X API write adapter for publishing posts

use async_trait::async_trait;
use news_lens_domain::{PublishError, PublishResult, Publisher, RenderedPost, XPublishMode};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// X API publisher for creating posts
pub struct XPublisher {
    client: Client,
    user_token: SecretString,
    base_url: String,
    mode: XPublishMode,
    max_chars: usize,
    enabled: bool,
}

impl XPublisher {
    pub fn new(user_token: SecretString, mode: XPublishMode, max_chars: usize) -> Self {
        Self::with_base_url(
            user_token,
            "https://api.twitter.com".to_string(),
            mode,
            max_chars,
            true,
        )
    }

    pub fn with_base_url(
        user_token: SecretString,
        base_url: String,
        mode: XPublishMode,
        max_chars: usize,
        enabled: bool,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            user_token,
            base_url,
            mode,
            max_chars,
            enabled,
        }
    }

    /// Create a disabled publisher (for testing/dry-run)
    pub fn disabled() -> Self {
        Self {
            client: Client::new(),
            user_token: SecretString::new("".into()),
            base_url: String::new(),
            mode: XPublishMode::default(),
            max_chars: 280,
            enabled: false,
        }
    }
}

#[derive(Serialize)]
struct CreateTweetRequest {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply: Option<ReplySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quote_tweet_id: Option<String>,
}

#[derive(Serialize)]
struct ReplySettings {
    in_reply_to_tweet_id: String,
}

#[derive(Deserialize)]
struct CreateTweetResponse {
    data: TweetData,
}

#[derive(Deserialize)]
struct TweetData {
    id: String,
}

#[async_trait]
impl Publisher for XPublisher {
    async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
        if !self.enabled {
            return Err(PublishError::Api("Publisher is disabled".to_string()));
        }

        let text = match self.mode {
            XPublishMode::NewPost => text_with_source_link(post),
            XPublishMode::Reply | XPublishMode::Quote => post.text.clone(),
        };

        // Validate content length
        if text.len() > self.max_chars {
            return Err(PublishError::ContentTooLong {
                len: text.len(),
                max: self.max_chars,
            });
        }

        let request = match self.mode {
            XPublishMode::Reply => CreateTweetRequest {
                text,
                reply: Some(ReplySettings {
                    in_reply_to_tweet_id: post.source_post_id.clone(),
                }),
                quote_tweet_id: None,
            },
            XPublishMode::Quote => CreateTweetRequest {
                text,
                reply: None,
                quote_tweet_id: Some(post.source_post_id.clone()),
            },
            XPublishMode::NewPost => CreateTweetRequest {
                text,
                reply: None,
                quote_tweet_id: None,
            },
        };

        let url = format!("{}/2/tweets", self.base_url);

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.user_token.expose_secret()),
            )
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| PublishError::Api(e.to_string()))?;

        if response.status() == 401 {
            return Err(PublishError::Auth("Invalid user token".to_string()));
        }

        if response.status() == 429 {
            return Err(PublishError::RateLimited);
        }

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PublishError::Api(format!(
                "Failed to create tweet: {}",
                body
            )));
        }

        let tweet_response: CreateTweetResponse = response
            .json()
            .await
            .map_err(|e| PublishError::Api(e.to_string()))?;

        // Note: We can't easily determine the author from the response
        // The URL format assumes we know the author, which we don't have here
        // A real implementation would need to also fetch user info
        Ok(PublishResult {
            id: tweet_response.data.id.clone(),
            url: Some(format!("https://x.com/i/status/{}", tweet_response.data.id)),
        })
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn platform(&self) -> &'static str {
        "x"
    }
}

fn text_with_source_link(post: &RenderedPost) -> String {
    let source_url = post.source_post_url.trim();
    if source_url.is_empty() {
        post.text.clone()
    } else if post.text.trim().is_empty() {
        source_url.to_string()
    } else {
        format!("{}\n\n{}", post.text.trim_end(), source_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_post() -> RenderedPost {
        RenderedPost {
            text: "Tags: test_tag (0.85)\nTest rationale".to_string(),
            source_post_id: "original_tweet_id".to_string(),
            source_post_url: "https://x.com/user/status/original_tweet_id".to_string(),
        }
    }

    #[tokio::test]
    async fn test_publish_reply_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/2/tweets"))
            .and(header("Authorization", "Bearer test-token"))
            .and(body_json(serde_json::json!({
                "text": "Tags: test_tag (0.85)\nTest rationale",
                "reply": {
                    "in_reply_to_tweet_id": "original_tweet_id"
                }
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "id": "new_tweet_id"
                }
            })))
            .mount(&mock_server)
            .await;

        let publisher = XPublisher::with_base_url(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            XPublishMode::Reply,
            280,
            true,
        );

        let result = publisher.publish(&sample_post()).await.unwrap();

        assert_eq!(result.id, "new_tweet_id");
        assert!(result.url.is_some());
    }

    #[tokio::test]
    async fn test_publish_quote_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/2/tweets"))
            .and(body_json(serde_json::json!({
                "text": "Tags: test_tag (0.85)\nTest rationale",
                "quote_tweet_id": "original_tweet_id"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "id": "quoted_tweet_id"
                }
            })))
            .mount(&mock_server)
            .await;

        let publisher = XPublisher::with_base_url(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            XPublishMode::Quote,
            280,
            true,
        );

        let result = publisher.publish(&sample_post()).await.unwrap();

        assert_eq!(result.id, "quoted_tweet_id");
    }

    #[tokio::test]
    async fn test_publish_new_post_includes_source_link() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/2/tweets"))
            .and(body_json(serde_json::json!({
                "text": "Tags: test_tag (0.85)\nTest rationale\n\nhttps://x.com/user/status/original_tweet_id"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "data": {
                    "id": "new_post_id"
                }
            })))
            .mount(&mock_server)
            .await;

        let publisher = XPublisher::with_base_url(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            XPublishMode::NewPost,
            280,
            true,
        );

        let result = publisher.publish(&sample_post()).await.unwrap();

        assert_eq!(result.id, "new_post_id");
    }

    #[tokio::test]
    async fn test_publish_content_too_long() {
        let publisher = XPublisher::with_base_url(
            SecretString::new("test-token".into()),
            "https://api.twitter.com".to_string(),
            XPublishMode::Reply,
            10, // Very short limit to trigger error
            true,
        );

        let result = publisher.publish(&sample_post()).await;

        assert!(matches!(result, Err(PublishError::ContentTooLong { .. })));
    }

    #[tokio::test]
    async fn test_publish_rate_limited() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/2/tweets"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let publisher = XPublisher::with_base_url(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            XPublishMode::Reply,
            280,
            true,
        );

        let result = publisher.publish(&sample_post()).await;

        assert!(matches!(result, Err(PublishError::RateLimited)));
    }

    #[tokio::test]
    async fn test_disabled_publisher() {
        let publisher = XPublisher::disabled();

        assert!(!publisher.is_enabled());

        let result = publisher.publish(&sample_post()).await;
        assert!(result.is_err());
    }
}
