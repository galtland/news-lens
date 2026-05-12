//! X API read adapter for fetching posts

use async_trait::async_trait;
use news_lens_domain::{PostFetchBatch, PostSource, PostSourceError, SourcePost, compare_post_ids};
use reqwest::{Client, Response};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;

const DEFAULT_TIMELINE_PAGE_CAP: usize = 5;
const MAX_TIMELINE_PAGE_CAP: usize = 10;
const TIMELINE_CURSOR_PREFIX: &str = "news-lens:x-backfill:";

/// X API post source for reading user timelines
pub struct XPostSource {
    client: Client,
    bearer_token: SecretString,
    base_url: String,
    timeline_page_cap: usize,
}

impl XPostSource {
    pub fn new(bearer_token: SecretString) -> Self {
        Self::with_page_cap(bearer_token, DEFAULT_TIMELINE_PAGE_CAP)
    }

    pub fn with_page_cap(bearer_token: SecretString, timeline_page_cap: usize) -> Self {
        Self::with_base_url_and_page_cap(
            bearer_token,
            "https://api.twitter.com".to_string(),
            timeline_page_cap,
        )
    }

    pub fn with_base_url(bearer_token: SecretString, base_url: String) -> Self {
        Self::with_base_url_and_page_cap(bearer_token, base_url, DEFAULT_TIMELINE_PAGE_CAP)
    }

    pub fn with_base_url_and_page_cap(
        bearer_token: SecretString,
        base_url: String,
        timeline_page_cap: usize,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            bearer_token,
            base_url,
            timeline_page_cap: timeline_page_cap.clamp(1, MAX_TIMELINE_PAGE_CAP),
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

        let user_response: UserResponse = parse_x_response(response, "users/by/username").await?;

        Ok(user_response.data.id)
    }

    /// Fetch tweets for a user
    async fn fetch_user_tweets(
        &self,
        user_id: &str,
        username: &str,
        cursor: &TimelineCursor,
    ) -> Result<TimelineFetch, PostSourceError> {
        let mut posts = Vec::new();
        let since_id = cursor.since_id.clone();
        let mut pagination_token = cursor.pagination_token.clone();
        let mut newest_seen_id = cursor.newest_seen_id.clone();
        let mut pages_fetched = 0usize;

        loop {
            let tweets_response = self
                .fetch_user_tweets_page(user_id, since_id.as_deref(), pagination_token.as_deref())
                .await?;
            pages_fetched += 1;

            let page_posts = tweets_response
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|tweet| source_post_from_tweet(tweet, Some(username)))
                .collect::<Vec<_>>();
            newest_seen_id = max_post_id(newest_seen_id, &page_posts);
            posts.extend(page_posts);

            let next_token = tweets_response
                .meta
                .and_then(|meta| meta.next_token)
                .filter(|token| !token.is_empty());
            let Some(next_token) = next_token else {
                break;
            };
            if pages_fetched >= self.timeline_page_cap {
                let next_since_id = TimelineCursor {
                    since_id: since_id.clone(),
                    pagination_token: Some(next_token),
                    newest_seen_id,
                }
                .encode()?;
                tracing::info!(
                    account = %username,
                    pages_fetched,
                    page_cap = self.timeline_page_cap,
                    "X tweet pagination cap reached; stored cursor will resume before since_id catches up"
                );
                return Ok(TimelineFetch {
                    posts,
                    next_since_id: Some(next_since_id),
                });
            }

            pagination_token = Some(next_token);
        }

        Ok(TimelineFetch {
            posts,
            next_since_id: newest_seen_id.or(since_id),
        })
    }

    async fn fetch_user_tweets_page(
        &self,
        user_id: &str,
        since_id: Option<&str>,
        pagination_token: Option<&str>,
    ) -> Result<TweetsResponse, PostSourceError> {
        let url = format!("{}/2/users/{}/tweets", self.base_url, user_id);
        let mut query = vec![
            ("tweet.fields", "created_at,referenced_tweets"),
            ("max_results", "100"),
        ];

        if let Some(since_id) = since_id {
            query.push(("since_id", since_id));
        }

        if let Some(pagination_token) = pagination_token {
            query.push(("pagination_token", pagination_token));
        }

        let response = self
            .client
            .get(&url)
            .query(&query)
            .header(
                "Authorization",
                format!("Bearer {}", self.bearer_token.expose_secret()),
            )
            .send()
            .await
            .map_err(|e| PostSourceError::Network(e.to_string()))?;

        parse_x_response(response, "users/:id/tweets").await
    }

    async fn fetch_posts_batch_for_user(
        &self,
        account: &str,
        user_id: &str,
        since_id: Option<&str>,
    ) -> Result<PostFetchBatch, PostSourceError> {
        let cursor = TimelineCursor::from_state(since_id)?;
        tracing::info!(account = %account, since_id = ?cursor.since_id, "Fetching posts from X");

        let TimelineFetch {
            mut posts,
            next_since_id,
        } = self.fetch_user_tweets(user_id, account, &cursor).await?;

        posts.sort_by(|a, b| compare_post_ids(&a.id, &b.id));

        tracing::info!(account = %account, count = posts.len(), "Fetched posts");

        Ok(PostFetchBatch {
            posts,
            next_since_id,
        })
    }

    /// Fetch a tweet directly by ID.
    async fn fetch_tweet_by_id(
        &self,
        post_id: &str,
    ) -> Result<Option<SourcePost>, PostSourceError> {
        let url = format!(
            "{}/2/tweets/{}?tweet.fields=created_at,referenced_tweets,author_id&expansions=author_id&user.fields=username",
            self.base_url, post_id
        );

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

        if response.status() == 404 {
            return Ok(None);
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
                "Failed to get tweet: {}",
                body
            )));
        }

        let tweet_response: TweetResponse = response
            .json()
            .await
            .map_err(|e| PostSourceError::Api(e.to_string()))?;

        let TweetResponse { data, includes } = tweet_response;
        let Some(tweet) = data else {
            return Ok(None);
        };

        let username = tweet
            .author_id
            .as_ref()
            .and_then(|author_id| username_for_author(includes.as_ref(), author_id));

        Ok(Some(source_post_from_tweet(tweet, username)))
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
    meta: Option<TweetsMeta>,
}

#[derive(Deserialize)]
struct TweetsMeta {
    next_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineCursor {
    since_id: Option<String>,
    pagination_token: Option<String>,
    newest_seen_id: Option<String>,
}

struct TimelineFetch {
    posts: Vec<SourcePost>,
    next_since_id: Option<String>,
}

impl TimelineCursor {
    fn from_state(value: Option<&str>) -> Result<Self, PostSourceError> {
        let Some(value) = value else {
            return Ok(Self {
                since_id: None,
                pagination_token: None,
                newest_seen_id: None,
            });
        };

        if let Some(json) = value.strip_prefix(TIMELINE_CURSOR_PREFIX) {
            return serde_json::from_str(json).map_err(|error| {
                PostSourceError::Api(format!("Invalid X pagination cursor: {}", error))
            });
        }

        Ok(Self {
            since_id: Some(value.to_string()),
            pagination_token: None,
            newest_seen_id: None,
        })
    }

    fn encode(&self) -> Result<String, PostSourceError> {
        serde_json::to_string(self)
            .map(|json| format!("{}{}", TIMELINE_CURSOR_PREFIX, json))
            .map_err(|error| {
                PostSourceError::Api(format!("Failed to encode X pagination cursor: {}", error))
            })
    }
}

#[derive(Deserialize)]
struct TweetResponse {
    data: Option<Tweet>,
    includes: Option<TweetIncludes>,
}

#[derive(Deserialize)]
struct TweetIncludes {
    users: Option<Vec<IncludedUser>>,
}

#[derive(Deserialize)]
struct IncludedUser {
    id: String,
    username: String,
}

#[derive(Deserialize)]
struct Tweet {
    id: String,
    text: String,
    author_id: Option<String>,
    created_at: Option<String>,
    referenced_tweets: Option<Vec<ReferencedTweet>>,
}

#[derive(Deserialize)]
struct ReferencedTweet {
    r#type: String,
    id: String,
}

fn username_for_author<'a>(
    includes: Option<&'a TweetIncludes>,
    author_id: &str,
) -> Option<&'a str> {
    includes?
        .users
        .as_ref()?
        .iter()
        .find(|user| user.id == author_id)
        .map(|user| user.username.as_str())
}

fn max_post_id(mut current: Option<String>, posts: &[SourcePost]) -> Option<String> {
    for post in posts {
        let should_replace = match current.as_deref() {
            Some(current) => compare_post_ids(current, &post.id).is_lt(),
            None => true,
        };
        if should_replace {
            current = Some(post.id.clone());
        }
    }
    current
}

async fn parse_x_response<T: DeserializeOwned>(
    response: Response,
    resource: &str,
) -> Result<T, PostSourceError> {
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
            "Failed to get {}: {}",
            resource, body
        )));
    }

    response
        .json()
        .await
        .map_err(|e| PostSourceError::Api(e.to_string()))
}

fn source_post_from_tweet(tweet: Tweet, username: Option<&str>) -> SourcePost {
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
        .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_else(OffsetDateTime::now_utc);

    let author = username
        .map(str::to_string)
        .or_else(|| tweet.author_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let url = username
        .map(|username| format!("https://x.com/{}/status/{}", username, tweet.id))
        .unwrap_or_else(|| format!("https://x.com/i/status/{}", tweet.id));

    SourcePost {
        id: tweet.id,
        text: tweet.text,
        author,
        url,
        created_at,
        is_repost,
        is_reply,
        reply_to_id,
    }
}

#[async_trait]
impl PostSource for XPostSource {
    async fn fetch_posts(
        &self,
        account: &str,
        since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError> {
        let mut posts = Vec::new();
        let mut next_since_id = since_id.map(str::to_string);
        let user_id = self.get_user_id(account).await?;

        loop {
            let batch = self
                .fetch_posts_batch_for_user(account, &user_id, next_since_id.as_deref())
                .await?;
            posts.extend(batch.posts);

            let Some(batch_next_since_id) = batch.next_since_id else {
                break;
            };
            if !batch_next_since_id.starts_with(TIMELINE_CURSOR_PREFIX) {
                break;
            }
            next_since_id = Some(batch_next_since_id);
        }

        posts.sort_by(|a, b| compare_post_ids(&a.id, &b.id));
        Ok(posts)
    }

    async fn fetch_posts_batch(
        &self,
        account: &str,
        since_id: Option<&str>,
    ) -> Result<PostFetchBatch, PostSourceError> {
        let user_id = self.get_user_id(account).await?;
        self.fetch_posts_batch_for_user(account, &user_id, since_id)
            .await
    }

    async fn fetch_post_by_id(&self, post_id: &str) -> Result<Option<SourcePost>, PostSourceError> {
        tracing::info!(post_id = %post_id, "Fetching post from X by ID");
        self.fetch_tweet_by_id(post_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{
        header, method, path, path_regex, query_param, query_param_is_missing,
    };
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
            .expect(1)
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
    async fn test_fetch_posts_follows_pagination() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/2/users/by/username/testuser"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "id": "123456789"
                }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param_is_missing("pagination_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "tweet1",
                        "text": "First page",
                        "created_at": "2024-01-15T12:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-2"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "tweet2",
                        "text": "Second page",
                        "created_at": "2024-01-15T13:00:00Z"
                    }
                ],
                "meta": {}
            })))
            .mount(&mock_server)
            .await;

        let source =
            XPostSource::with_base_url(SecretString::new("test-token".into()), mock_server.uri());

        let posts = source.fetch_posts("testuser", None).await.unwrap();

        assert_eq!(
            posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tweet1", "tweet2"]
        );
    }

    #[tokio::test]
    async fn test_direct_fetch_posts_fetches_past_page_cap() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/2/users/by/username/testuser"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "id": "123456789"
                }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param_is_missing("pagination_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "300",
                        "text": "First page",
                        "created_at": "2024-01-15T14:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-2"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "200",
                        "text": "Second page",
                        "created_at": "2024-01-15T13:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-3"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "100",
                        "text": "Third page",
                        "created_at": "2024-01-15T12:00:00Z"
                    }
                ],
                "meta": {}
            })))
            .mount(&mock_server)
            .await;

        let source = XPostSource::with_base_url_and_page_cap(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            2,
        );

        let posts = source.fetch_posts("testuser", None).await.unwrap();

        assert_eq!(
            posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["100", "200", "300"]
        );
    }

    #[tokio::test]
    async fn test_fetch_posts_stops_at_page_cap() {
        let mock_server = MockServer::start().await;

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

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param_is_missing("pagination_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "tweet1",
                        "text": "First page",
                        "created_at": "2024-01-15T12:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-2"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "tweet2",
                        "text": "Second page",
                        "created_at": "2024-01-15T13:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-3"
                }
            })))
            .mount(&mock_server)
            .await;

        let source = XPostSource::with_base_url_and_page_cap(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            2,
        );

        let batch = source.fetch_posts_batch("testuser", None).await.unwrap();

        assert_eq!(
            batch
                .posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tweet1", "tweet2"]
        );
        assert!(
            batch
                .next_since_id
                .as_deref()
                .is_some_and(|cursor| cursor.starts_with(TIMELINE_CURSOR_PREFIX))
        );
    }

    #[tokio::test]
    async fn test_fetch_posts_resumes_capped_pagination_cursor() {
        let mock_server = MockServer::start().await;

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

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param_is_missing("pagination_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "300",
                        "text": "First page",
                        "created_at": "2024-01-15T14:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-2"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "200",
                        "text": "Second page",
                        "created_at": "2024-01-15T13:00:00Z"
                    }
                ],
                "meta": {
                    "next_token": "page-3"
                }
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/2/users/123456789/tweets"))
            .and(query_param("pagination_token", "page-3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "100",
                        "text": "Third page",
                        "created_at": "2024-01-15T12:00:00Z"
                    }
                ],
                "meta": {}
            })))
            .mount(&mock_server)
            .await;

        let source = XPostSource::with_base_url_and_page_cap(
            SecretString::new("test-token".into()),
            mock_server.uri(),
            2,
        );

        let first = source.fetch_posts_batch("testuser", None).await.unwrap();
        let cursor = first.next_since_id.expect("pagination cursor");

        assert_eq!(
            first
                .posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["200", "300"]
        );

        let resumed = source
            .fetch_posts_batch("testuser", Some(cursor.as_str()))
            .await
            .unwrap();

        assert_eq!(
            resumed
                .posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["100"]
        );
        assert_eq!(resumed.next_since_id.as_deref(), Some("300"));
    }

    #[tokio::test]
    async fn test_fetch_post_by_id_success() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/2/tweets/tweet1"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "id": "tweet1",
                    "text": "Hello world",
                    "author_id": "user1",
                    "created_at": "2024-01-15T12:00:00Z"
                },
                "includes": {
                    "users": [
                        {"id": "user1", "username": "testuser"}
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let source =
            XPostSource::with_base_url(SecretString::new("test-token".into()), mock_server.uri());

        let post = source
            .fetch_post_by_id("tweet1")
            .await
            .expect("lookup")
            .expect("post");

        assert_eq!(post.id, "tweet1");
        assert_eq!(post.author, "testuser");
        assert_eq!(post.url, "https://x.com/testuser/status/tweet1");
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
