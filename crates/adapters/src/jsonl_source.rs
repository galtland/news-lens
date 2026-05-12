//! JSONL file-based post source adapter

use async_trait::async_trait;
use news_lens_domain::{PostSource, PostSourceError, SourcePost, compare_post_ids};
use std::path::PathBuf;

/// Post source that reads SourcePost entries from a JSONL file
pub struct JsonlPostSource {
    paths: Vec<PathBuf>,
}

impl JsonlPostSource {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    fn load_posts(&self) -> Result<Vec<SourcePost>, PostSourceError> {
        let mut posts = Vec::new();
        for path in &self.paths {
            let content = std::fs::read_to_string(path).map_err(|e| {
                PostSourceError::Api(format!("Failed to read {}: {}", path.display(), e))
            })?;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // Skip non-SourcePost lines (e.g. cursor entries from fetch)
                if let Ok(post) = serde_json::from_str::<SourcePost>(line) {
                    posts.push(post);
                }
            }
        }
        posts.sort_by(|a, b| compare_post_ids(&a.id, &b.id));
        Ok(posts)
    }
}

#[async_trait]
impl PostSource for JsonlPostSource {
    async fn fetch_posts(
        &self,
        account: &str,
        since_id: Option<&str>,
    ) -> Result<Vec<SourcePost>, PostSourceError> {
        let posts = self.load_posts()?;
        let filtered: Vec<SourcePost> = posts
            .into_iter()
            .filter(|p| account == "*" || p.author.eq_ignore_ascii_case(account))
            .filter(|p| {
                since_id
                    .map(|sid| compare_post_ids(&p.id, sid).is_gt())
                    .unwrap_or(true)
            })
            .collect();
        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn make_post(id: &str, author: &str) -> SourcePost {
        SourcePost {
            id: id.to_string(),
            text: "text".to_string(),
            author: author.to_string(),
            url: "https://example.com".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            is_repost: false,
            is_reply: false,
            reply_to_id: None,
        }
    }

    #[tokio::test]
    async fn fetch_posts_uses_numeric_id_ordering_for_since_filter() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("posts.jsonl");
        let lines = [
            serde_json::to_string(&make_post("9", "alice")).unwrap(),
            serde_json::to_string(&make_post("10", "alice")).unwrap(),
        ]
        .join("\n");
        std::fs::write(&file_path, format!("{}\n", lines)).unwrap();

        let source = JsonlPostSource::new(vec![file_path]);
        let posts = source.fetch_posts("alice", Some("9")).await.unwrap();

        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].id, "10");
    }
}
