//! Outbox publisher for require-approval mode.

use crate::x_api::format_new_post_text;
use async_trait::async_trait;
use news_lens_domain::model::{RenderedPost, XPublishMode};
use news_lens_domain::ports::{PublishError, PublishResult, Publisher};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct OutboxWriter {
    path: PathBuf,
    file: Arc<Mutex<tokio::fs::File>>,
}

impl OutboxWriter {
    pub async fn new(path: PathBuf) -> Result<Self, OutboxError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).await?;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        Ok(Self {
            path,
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn append(&self, entry: &OutboxEntry<'_>) -> Result<(), OutboxError> {
        let line = serde_json::to_string(entry)?;
        let mut file = self.file.lock().await;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct OutboxPublisher {
    writer: OutboxWriter,
    platform: &'static str,
    text_mode: OutboxTextMode,
}

impl OutboxPublisher {
    pub fn new(writer: OutboxWriter, platform: &'static str) -> Self {
        Self {
            writer,
            platform,
            text_mode: OutboxTextMode::Plain,
        }
    }

    pub fn new_x(writer: OutboxWriter, mode: XPublishMode, max_chars: usize) -> Self {
        let text_mode = match mode {
            XPublishMode::NewPost => OutboxTextMode::XNewPost { max_chars },
            XPublishMode::Reply | XPublishMode::Quote => OutboxTextMode::Plain,
        };

        Self {
            writer,
            platform: "x",
            text_mode,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum OutboxTextMode {
    Plain,
    XNewPost { max_chars: usize },
}

#[derive(Serialize)]
struct OutboxEntry<'a> {
    platform: &'a str,
    source_post_id: &'a str,
    source_post_url: &'a str,
    text: &'a str,
}

#[async_trait]
impl Publisher for OutboxPublisher {
    async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
        let text = match self.text_mode {
            OutboxTextMode::Plain => post.text.clone(),
            OutboxTextMode::XNewPost { max_chars } => format_new_post_text(post, max_chars),
        };
        let entry = OutboxEntry {
            platform: self.platform,
            source_post_id: &post.source_post_id,
            source_post_url: &post.source_post_url,
            text: &text,
        };

        self.writer
            .append(&entry)
            .await
            .map_err(|error| PublishError::Api(format!("Outbox write failed: {}", error)))?;

        Ok(PublishResult {
            id: None,
            url: None,
        })
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn platform(&self) -> &'static str {
        self.platform
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    #[tokio::test]
    async fn outbox_publisher_writes_jsonl_entry() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("outbox.jsonl");

        let writer = OutboxWriter::new(path.clone()).await.expect("writer");
        let publisher = OutboxPublisher::new(writer, "x");

        let post = RenderedPost {
            text: "Rendered content".to_string(),
            source_post_id: "123".to_string(),
            source_post_url: "https://x.com/example/status/123".to_string(),
        };

        let result = publisher.publish(&post).await.expect("publish");
        assert!(result.id.is_none());

        let contents = tokio::fs::read_to_string(&path).await.expect("read outbox");
        let line = contents.trim();
        let value: Value = serde_json::from_str(line).expect("valid json");

        assert_eq!(value["platform"], "x");
        assert_eq!(value["source_post_id"], "123");
        assert_eq!(value["source_post_url"], "https://x.com/example/status/123");
        assert_eq!(value["text"], "Rendered content");
    }

    #[tokio::test]
    async fn x_new_post_outbox_entry_includes_source_link() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("outbox.jsonl");

        let writer = OutboxWriter::new(path.clone()).await.expect("writer");
        let publisher = OutboxPublisher::new_x(writer, XPublishMode::NewPost, 280);

        let post = RenderedPost {
            text: "Rendered content".to_string(),
            source_post_id: "123".to_string(),
            source_post_url: "https://x.com/example/status/123".to_string(),
        };

        publisher.publish(&post).await.expect("publish");

        let contents = tokio::fs::read_to_string(&path).await.expect("read outbox");
        let value: Value = serde_json::from_str(contents.trim()).expect("valid json");

        assert_eq!(
            value["text"],
            "Rendered content\n\nhttps://x.com/example/status/123"
        );
    }
}
