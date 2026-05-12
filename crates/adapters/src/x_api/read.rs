//! X API read adapter for fetching posts

use async_trait::async_trait;
use news_lens_domain::{PostSource, PostSourceError, SourcePost, compare_post_ids};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::time::Duration;
use time::OffsetDateTime;

/// X API post source for reading user timelines
pub struct XPostSource {
    client: Client,
    bearer_token: SecretString,
    base_url: String,
}

impl XPostSource {
    pub fn new(bearer_token: SecretString) -> Self {
        Self::with_base_url(bearer_token, "https://api.twitter.com".to_string())
    }

    pub fn with_base_url(bearer_token: SecretString, base_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            bearer_token,
            base_url,
        }
    }

    /// Look up user ID by username
    async fn get_user_id(&self, username: &str) -> Result<String, PostSourceError> {
        let url = format!("{}/2/users/by/username/{}", self.base_url, username);

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.bearer_token.expose_secret()),
            )
            .send()
            .await
            .map_err(|e| PostSourceError::Network(e.to_string()))?;

        if response.status() == 401 {
            return Err(PostSourceError::Auth("Invalid bearer token".to_string()));
        }

        if response.status() == 429 {
            let retry_after = response
                .headers()
                .get("x-rate-limit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ts| {
                    let now = OffsetDateTime::now_utc().unix_timestamp() as u64;
                    Duration::from_secs(ts.saturating_sub(now))
                });
            return Err(PostSourceError::RateLimited(retry_after));
        }

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PostSourceError::Api(format!(
                "Failed to get user: {}",
                body
            )));
        }

        let user_response: UserResponse = response
            .json()
            .await
            .map_err(|e| PostSourceError::Api(e.to_string()))?;

        Ok(user_response.data.id)
    }

    /// Fetch tweets for a user
    async fn fetch_user_tweets(
        &self,
        user_id: &str,
        username: &str,
        since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError> {
        let mut url = format!(
            "{}/2/users/{}/tweets?tweet.fields=created_at,referenced_tweets&max_results=100",
            self.base_url, user_id
        );

        if let Some(since_id) = since_id {
            url.push_str(&format!("&since_id={}", since_id));
        }

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.bearer_token.expose_secret()),
            )
            .send()
            .await
            .map_err(|e| PostSourceError::Network(e.to_string()))?;

        if response.status() == 401 {
            return Err(PostSourceError::Auth("Invalid bearer token".to_string()));
        }

        if response.status() == 429 {
            let retry_after = response
                .headers()
                .get("x-rate-limit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ts| {
                    let now = OffsetDateTime::now_utc().unix_timestamp() as u64;
                    Duration::from_secs(ts.saturating_sub(now))
                });
            return Err(PostSourceError::RateLimited(retry_after));
        }

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PostSourceError::Api(format!(
                "Failed to get tweets: {}",
                body
            )));
        }

        let tweets_response: TweetsResponse = response
            .json()
            .await
            .map_err(|e| PostSourceError::Api(e.to_string()))?;

        let posts = tweets_response
            .data
            .unwrap_or_default()
            .into_iter()
            .map(|tweet| {
                let is_repost = tweet
                    .referenced_tweets
                    .as_ref()
                    .map(|refs| refs.iter().any(|r| r.r#type == "retweeted"))
                    .unwrap_or(false);

                let is_reply = tweet
                    .referenced_tweets
                    .as_ref()
                    .map(|refs| refs.iter().any(|r| r.r#type == "replied_to"))
                    .unwrap_or(false);

                let reply_to_id = tweet.referenced_tweets.as_ref().and_then(|refs| {
                    refs.iter()
                        .find(|r| r.r#type == "replied_to")
                        .map(|r| r.id.clone())
                });

                let created_at = tweet
                    .created_at
                    .as_ref()
                    .and_then(|s| {
                        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                            .ok()
                    })
                    .unwrap_or_else(OffsetDateTime::now_utc);

                SourcePost {
                    id: tweet.id.clone(),
                    text: tweet.text,
                    author: username.to_string(),
                    url: format!("https://x.com/{}/status/{}", username, tweet.id),
                    created_at,
                    is_repost,
                    is_reply,
                    reply_to_id,
                }
            })
            .collect();

        Ok(posts)
    }
}

#[derive(Deserialize)]
struct UserResponse {
    data: UserData,
}

#[derive(Deserialize)]
struct UserData {
    id: String,
}

#[derive(Deserialize)]
struct TweetsResponse {
    data: Option<Vec<Tweet>>,
}

#[derive(Deserialize)]
struct Tweet {
    id: String,
    text: String,
    created_at: Option<String>,
    referenced_tweets: Option<Vec<ReferencedTweet>>,
}

#[derive(Deserialize)]
struct ReferencedTweet {
    r#type: String,
    id: String,
}

#[async_trait]
impl PostSource for XPostSource {
    async fn fetch_posts(
        &self,
        account: &str,
        since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError> {
        tracing::info!(account = %account, since_id = ?since_id, "Fetching posts from X");

        // Get user ID from username
        let user_id = self.get_user_id(account).await?;

        // Fetch tweets
        let mut posts = self.fetch_user_tweets(&user_id, account, since_id).await?;

        // Sort by ID (which is chronological) ascending
        posts.sort_by(|a, b| compare_post_ids(&a.id, &b.id));

        tracing::info!(account = %account, count = posts.len(), "Fetched posts");

        Ok(posts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_fetch_posts_success() {
        let mock_server = MockServer::start().await;

        // Mock user lookup
        Mock::given(method("GET"))
            .and(path("/2/users/by/username/testuser"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "id": "123456789"
                }
            })))
            .mount(&mock_server)
            .await;

        // Mock tweets endpoint
        Mock::given(method("GET"))
            .and(path_regex(r"/2/users/123456789/tweets.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "tweet1",
                        "text": "Hello world",
                        "created_at": "2024-01-15T12:00:00Z"
                    },
                    {
                        "id": "tweet2",
                        "text": "Another post",
                        "created_at": "2024-01-15T13:00:00Z",
                        "referenced_tweets": [
                            {"type": "replied_to", "id": "tweet0"}
                        ]
                    }
                ]
            })))
            .mount(&mock_server)
            .await;

        let source =
            XPostSource::with_base_url(SecretString::new("test-token".into()), mock_server.uri());

        let posts = source.fetch_posts("testuser", None).await.unwrap();

        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].id, "tweet1");
        assert!(!posts[0].is_reply);
        assert_eq!(posts[1].id, "tweet2");
        assert!(posts[1].is_reply);
    }

    #[tokio::test]
    async fn test_fetch_posts_rate_limited() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/2/users/by/username/testuser"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let source =
            XPostSource::with_base_url(SecretString::new("test-token".into()), mock_server.uri());

        let result = source.fetch_posts("testuser", None).await;

        assert!(matches!(result, Err(PostSourceError::RateLimited(_))));
    }

    #[tokio::test]
    async fn test_fetch_posts_auth_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/2/users/by/username/testuser"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&mock_server)
            .await;

        let source =
            XPostSource::with_base_url(SecretString::new("bad-token".into()), mock_server.uri());

        let result = source.fetch_posts("testuser", None).await;

        assert!(matches!(result, Err(PostSourceError::Auth(_))));
    }
}
