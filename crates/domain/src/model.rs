//! Domain models and value objects.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use time::OffsetDateTime;

/// A source post from a watched platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePost {
    /// Platform-specific post ID.
    pub id: String,
    /// Post text content.
    pub text: String,
    /// Author username/handle.
    pub author: String,
    /// URL to the original post.
    pub url: String,
    /// When the post was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Whether this is a repost/retweet.
    pub is_repost: bool,
    /// Whether this is a reply.
    pub is_reply: bool,
    /// ID of post being replied to, if any.
    pub reply_to_id: Option<String>,
}

/// Active editorial lens loaded from markdown.
#[derive(Debug, Clone)]
pub struct Lens {
    pub id: String,
    pub voice: Option<String>,
    pub register: Option<String>,
    pub path: PathBuf,
    pub content: String,
}

/// Context passed to the subprocess harness for one post.
#[derive(Debug, Clone)]
pub struct PostContext {
    pub post: SourcePost,
    pub wiki_path: PathBuf,
    pub lens: Lens,
    pub candidate_slug: String,
}

/// Agent stance vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    Endorse,
    Critique,
    Contextualize,
    Decline,
    Failed,
}

impl Stance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Endorse => "endorse",
            Self::Critique => "critique",
            Self::Contextualize => "contextualize",
            Self::Decline => "decline",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for Stance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Stance {
    type Err = AgentValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "endorse" => Ok(Self::Endorse),
            "critique" => Ok(Self::Critique),
            "contextualize" => Ok(Self::Contextualize),
            "decline" => Ok(Self::Decline),
            "failed" => Ok(Self::Failed),
            _ => Err(AgentValidationError::InvalidStance(trimmed.to_string())),
        }
    }
}

/// Per-thread-item character limit (X's per-post limit).
pub const THREAD_ITEM_MAX_CHARS: usize = 280;

/// Hard cap on number of thread items. Defends against an agent that
/// emits a runaway thread of cited URLs. Generous — typical threads
/// are 2–4 items.
pub const MAX_THREAD_ITEMS: usize = 10;

/// Raw JSON shape printed by the agent before contract validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawAgentReturn {
    pub stance: Option<String>,
    pub raw_path: Option<String>,
    pub raw_slug: Option<String>,
    pub thesis_path: Option<String>,
    pub thesis_slug: Option<String>,
    pub thread: Option<Vec<String>>,
    /// Knowledge-gap entries verbatim from /wiki:query --deep, when the agent
    /// invoked it. Each entry is one gap line. Optional and survives any stance.
    pub gaps: Option<Vec<String>>,
}

/// Validated JSON return from the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReturn {
    pub stance: Stance,
    pub raw_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thesis_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thesis_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Vec<String>>,
}

impl RawAgentReturn {
    /// Validate the agent contract from spec section 9.
    pub fn validate(self, wiki_root: &Path) -> Result<AgentReturn, AgentValidationError> {
        let stance: Stance = self
            .stance
            .ok_or(AgentValidationError::MissingField("stance"))?
            .parse()?;

        let raw_path = self
            .raw_path
            .ok_or(AgentValidationError::MissingField("raw_path"))?;
        let raw_path = normalize_existing_wiki_file(wiki_root, "raw_path", &raw_path)?;

        let validated_thread = self.thread.map(validate_thread_items).transpose()?;
        let validated_gaps = self.gaps.map(sanitize_gaps).filter(|g| !g.is_empty());

        if matches!(
            stance,
            Stance::Endorse | Stance::Critique | Stance::Contextualize
        ) {
            let thesis_path = self
                .thesis_path
                .ok_or(AgentValidationError::MissingField("thesis_path"))?;
            let thesis_path = normalize_existing_wiki_file(wiki_root, "thesis_path", &thesis_path)?;

            let thesis_slug = self
                .thesis_slug
                .ok_or(AgentValidationError::MissingField("thesis_slug"))?;
            let thesis_slug = thesis_slug.trim().to_string();
            if thesis_slug.is_empty() {
                return Err(AgentValidationError::BlankThesisSlug);
            }
            let thread = validated_thread.ok_or(AgentValidationError::MissingField("thread"))?;
            if thread.is_empty() {
                return Err(AgentValidationError::EmptyThread);
            }

            return Ok(AgentReturn {
                stance,
                raw_path,
                raw_slug: self.raw_slug,
                thesis_path: Some(thesis_path),
                thesis_slug: Some(thesis_slug),
                thread: Some(thread),
                gaps: validated_gaps,
            });
        }

        let thesis_path = self
            .thesis_path
            .map(|path| normalize_existing_wiki_file(wiki_root, "thesis_path", &path))
            .transpose()?;

        let thesis_slug = self
            .thesis_slug
            .map(|slug| slug.trim().to_string())
            .filter(|slug| !slug.is_empty());

        // For decline/failed, an empty array is dropped to None for cleanliness.
        let thread = validated_thread.filter(|items| !items.is_empty());

        Ok(AgentReturn {
            stance,
            raw_path,
            raw_slug: self.raw_slug,
            thesis_path,
            thesis_slug,
            thread,
            gaps: validated_gaps,
        })
    }
}

/// Trim, drop blanks, and dedupe gap entries verbatim from /wiki:query.
fn sanitize_gaps(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

fn validate_thread_items(items: Vec<String>) -> Result<Vec<String>, AgentValidationError> {
    if items.len() > MAX_THREAD_ITEMS {
        return Err(AgentValidationError::ThreadTooLong {
            len: items.len(),
            max: MAX_THREAD_ITEMS,
        });
    }
    let mut validated = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let trimmed = item.trim().to_string();
        if trimmed.is_empty() {
            return Err(AgentValidationError::BlankThreadItem { index });
        }
        let len = trimmed.chars().count();
        if len > THREAD_ITEM_MAX_CHARS {
            return Err(AgentValidationError::ThreadItemTooLong {
                index,
                len,
                max: THREAD_ITEM_MAX_CHARS,
            });
        }
        validated.push(trimmed);
    }
    Ok(validated)
}

/// Validation errors for the agent return contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentValidationError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("invalid stance: {0}")]
    InvalidStance(String),
    #[error("{field} is empty")]
    EmptyPath { field: &'static str },
    #[error("{field} points outside the wiki root: {path}")]
    OutsideWikiRoot { field: &'static str, path: String },
    #[error("wiki root does not exist or is unreadable: {path}")]
    MissingWikiRoot { path: String },
    #[error("{field} does not exist: {path}")]
    MissingFile { field: &'static str, path: String },
    #[error("thesis_slug is blank")]
    BlankThesisSlug,
    #[error("thread is empty")]
    EmptyThread,
    #[error("thread[{index}] is blank")]
    BlankThreadItem { index: usize },
    #[error("thread[{index}] is {len} chars, exceeds limit of {max}")]
    ThreadItemTooLong {
        index: usize,
        len: usize,
        max: usize,
    },
    #[error("thread has {len} items, exceeds limit of {max}")]
    ThreadTooLong { len: usize, max: usize },
}

/// Publishing mode for X posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum XPublishMode {
    /// Reply to the original post.
    #[default]
    Reply,
    /// Quote the original post.
    Quote,
    /// Create a standalone post with link.
    NewPost,
}

/// Rendered content ready for publishing.
#[derive(Debug, Clone)]
pub struct RenderedPost {
    pub text: String,
    pub source_post_id: String,
    pub source_post_url: String,
    /// When `Some`, this post must be published as a direct reply to the given
    /// platform-specific post ID. Set by the run loop while chaining thread items.
    pub in_reply_to_id: Option<String>,
}

/// Account watch state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    pub account: String,
    pub since_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Record of what happened to one source post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedPostRecord {
    pub post_id: String,
    pub lens_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub processed_at: OffsetDateTime,
    pub stance: Stance,
    pub raw_path: Option<String>,
    pub thesis_slug: Option<String>,
    pub x_post_id: Option<String>,
    pub nostr_event_id: Option<String>,
    /// Knowledge gaps surfaced by /wiki:query during this run, persisted so the
    /// `news-lens gaps` subcommand can list them later. Empty/None when no gaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Vec<String>>,
}

/// Processing result for a single post.
#[derive(Debug)]
pub enum ProcessResult {
    /// Post was processed by the harness and recorded.
    Processed {
        source_post: Box<SourcePost>,
        agent_return: AgentReturn,
        x_post_id: Option<String>,
        nostr_event_id: Option<String>,
    },
    /// Post was skipped (already processed, filtered, etc.).
    Skipped { reason: String },
    /// Harness, validation, publishing, or recording failed.
    Failed { error: String },
}

pub(crate) fn normalize_existing_wiki_file(
    wiki_root: &Path,
    field: &'static str,
    value: &str,
) -> Result<String, AgentValidationError> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(AgentValidationError::EmptyPath { field });
    }

    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AgentValidationError::OutsideWikiRoot {
            field,
            path: value.to_string(),
        });
    }

    let wiki_root =
        wiki_root
            .canonicalize()
            .map_err(|_| AgentValidationError::MissingWikiRoot {
                path: wiki_root.display().to_string(),
            })?;

    let canonical =
        wiki_root
            .join(path)
            .canonicalize()
            .map_err(|_| AgentValidationError::MissingFile {
                field,
                path: value.to_string(),
            })?;

    if !canonical.starts_with(&wiki_root) {
        return Err(AgentValidationError::OutsideWikiRoot {
            field,
            path: value.to_string(),
        });
    }

    if !canonical.is_file() || canonical.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return Err(AgentValidationError::MissingFile {
            field,
            path: value.to_string(),
        });
    }

    let relative =
        canonical
            .strip_prefix(&wiki_root)
            .map_err(|_| AgentValidationError::OutsideWikiRoot {
                field,
                path: value.to_string(),
            })?;

    Ok(relative.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiki_with_files() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("raw/news")).expect("raw dir");
        std::fs::create_dir_all(dir.path().join("wiki/theses")).expect("theses dir");
        std::fs::write(dir.path().join("raw/news/item.md"), "# News").expect("raw file");
        std::fs::write(dir.path().join("wiki/theses/item.md"), "# Thesis").expect("thesis file");
        dir
    }

    fn valid_raw() -> RawAgentReturn {
        RawAgentReturn {
            stance: Some("critique".to_string()),
            raw_path: Some("raw/news/item.md".to_string()),
            raw_slug: Some("item".to_string()),
            thesis_path: Some("wiki/theses/item.md".to_string()),
            thesis_slug: Some("item".to_string()),
            thread: Some(vec![
                "A concise analytic claim.".to_string(),
                "Sources: https://example.test/concepts/foo".to_string(),
            ]),
            gaps: None,
        }
    }

    #[test]
    fn validation_rejects_missing_field() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.raw_path = None;

        let error = raw.validate(wiki.path()).expect_err("missing raw_path");
        assert_eq!(error, AgentValidationError::MissingField("raw_path"));
    }

    #[test]
    fn validation_rejects_bad_stance_enum() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("maybe".to_string());

        let error = raw.validate(wiki.path()).expect_err("bad stance");
        assert_eq!(
            error,
            AgentValidationError::InvalidStance("maybe".to_string())
        );
    }

    #[test]
    fn validation_accepts_title_case_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("Critique".to_string());

        let output = raw.validate(wiki.path()).expect("valid stance");
        assert_eq!(output.stance, Stance::Critique);
    }

    #[test]
    fn validation_rejects_missing_thesis_path_on_non_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thesis_path = None;

        let error = raw.validate(wiki.path()).expect_err("missing thesis path");
        assert_eq!(error, AgentValidationError::MissingField("thesis_path"));
    }

    #[test]
    fn validation_accepts_failed_without_thesis_fields() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("failed".to_string());
        raw.thesis_path = None;
        raw.thesis_slug = None;
        raw.thread = None;

        let output = raw.validate(wiki.path()).expect("failed stance");
        assert_eq!(output.stance, Stance::Failed);
        assert!(output.thesis_path.is_none());
        assert!(output.thesis_slug.is_none());
        assert!(output.thread.is_none());
    }

    #[test]
    fn validation_rejects_missing_thread_on_non_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = None;

        let error = raw.validate(wiki.path()).expect_err("missing thread");
        assert_eq!(error, AgentValidationError::MissingField("thread"));
    }

    #[test]
    fn validation_rejects_empty_thread_on_non_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = Some(vec![]);

        let error = raw.validate(wiki.path()).expect_err("empty thread");
        assert_eq!(error, AgentValidationError::EmptyThread);
    }

    #[test]
    fn validation_rejects_blank_thread_item() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = Some(vec![
            "lead post".to_string(),
            "   \n\t ".to_string(),
            "sources".to_string(),
        ]);

        let error = raw.validate(wiki.path()).expect_err("blank thread item");
        assert_eq!(error, AgentValidationError::BlankThreadItem { index: 1 });
    }

    #[test]
    fn validation_rejects_thread_with_more_than_max_items() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = Some(
            (0..MAX_THREAD_ITEMS + 1)
                .map(|i| format!("item {}", i))
                .collect(),
        );

        let error = raw.validate(wiki.path()).expect_err("over-long thread");
        assert_eq!(
            error,
            AgentValidationError::ThreadTooLong {
                len: MAX_THREAD_ITEMS + 1,
                max: MAX_THREAD_ITEMS,
            }
        );
    }

    #[test]
    fn validation_rejects_thread_item_over_280_chars() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        // 281 ASCII chars -> 281 char count, just over the limit.
        let too_long = "a".repeat(281);
        raw.thread = Some(vec!["lead".to_string(), too_long]);

        let error = raw
            .validate(wiki.path())
            .expect_err("over-long thread item");
        assert_eq!(
            error,
            AgentValidationError::ThreadItemTooLong {
                index: 1,
                len: 281,
                max: THREAD_ITEM_MAX_CHARS,
            }
        );
    }

    #[test]
    fn validation_measures_thread_item_length_by_character_count() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        // 281 'é' chars: each is 2 bytes in UTF-8 (562 bytes) but the char count
        // is what counts against X's per-post limit.
        let multibyte = "é".repeat(281);
        raw.thread = Some(vec!["lead".to_string(), multibyte]);

        let error = raw
            .validate(wiki.path())
            .expect_err("over-long multibyte thread item");
        assert_eq!(
            error,
            AgentValidationError::ThreadItemTooLong {
                index: 1,
                len: 281,
                max: THREAD_ITEM_MAX_CHARS,
            }
        );
    }

    #[test]
    fn validation_accepts_thread_item_at_exact_limit() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = Some(vec!["a".repeat(THREAD_ITEM_MAX_CHARS)]);

        let output = raw.validate(wiki.path()).expect("at-limit thread item");
        let thread = output.thread.expect("thread present");
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].chars().count(), THREAD_ITEM_MAX_CHARS);
    }

    #[test]
    fn validation_trims_thread_items_before_recording() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thread = Some(vec![
            "  lead post  \n".to_string(),
            "\tsources \n".to_string(),
        ]);

        let output = raw.validate(wiki.path()).expect("trims thread");
        let thread = output.thread.expect("thread present");
        assert_eq!(thread, vec!["lead post".to_string(), "sources".to_string()]);
    }

    #[test]
    fn validation_rejects_blank_thesis_slug_on_non_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thesis_slug = Some(" \n\t ".to_string());

        let error = raw.validate(wiki.path()).expect_err("blank thesis_slug");
        assert_eq!(error, AgentValidationError::BlankThesisSlug);
    }

    #[test]
    fn validation_trims_thesis_slug_before_recording() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.thesis_slug = Some("  item \n".to_string());

        let output = raw.validate(wiki.path()).expect("valid thesis_slug");
        assert_eq!(output.thesis_slug.as_deref(), Some("item"));
    }

    #[test]
    fn validation_drops_blank_optional_thesis_slug_on_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("decline".to_string());
        raw.thesis_path = None;
        raw.thesis_slug = Some(" \n\t ".to_string());
        raw.thread = None;

        let output = raw.validate(wiki.path()).expect("valid decline");
        assert!(output.thesis_slug.is_none());
    }

    #[test]
    fn validation_trims_optional_thesis_slug_on_failed_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("failed".to_string());
        raw.thesis_path = Some("wiki/theses/item.md".to_string());
        raw.thesis_slug = Some(" item \n".to_string());
        raw.thread = None;

        let output = raw.validate(wiki.path()).expect("valid failed");
        assert_eq!(output.thesis_slug.as_deref(), Some("item"));
    }

    #[test]
    fn validation_validates_thread_items_on_decline_when_present() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("decline".to_string());
        raw.thesis_path = None;
        raw.thesis_slug = None;
        raw.thread = Some(vec!["a".repeat(281)]);

        let error = raw.validate(wiki.path()).expect_err("decline rejects long");
        assert!(matches!(
            error,
            AgentValidationError::ThreadItemTooLong { .. }
        ));
    }

    #[test]
    fn validation_drops_empty_optional_thread_on_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("decline".to_string());
        raw.thesis_path = None;
        raw.thesis_slug = None;
        raw.thread = Some(vec![]);

        let output = raw.validate(wiki.path()).expect("valid decline");
        assert!(output.thread.is_none());
    }

    #[test]
    fn validation_reports_missing_wiki_root_separately() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let missing_wiki = dir.path().join("missing-wiki");

        let error = valid_raw()
            .validate(&missing_wiki)
            .expect_err("missing wiki root");

        assert_eq!(
            error,
            AgentValidationError::MissingWikiRoot {
                path: missing_wiki.display().to_string()
            }
        );
    }

    #[test]
    fn validation_rejects_missing_raw_path_file() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.raw_path = Some("raw/news/missing.md".to_string());

        let error = raw.validate(wiki.path()).expect_err("missing raw file");
        assert_eq!(
            error,
            AgentValidationError::MissingFile {
                field: "raw_path",
                path: "raw/news/missing.md".to_string()
            }
        );
    }

    #[test]
    fn validation_rejects_empty_raw_path() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.raw_path = Some(String::new());

        let error = raw.validate(wiki.path()).expect_err("empty raw path");
        assert_eq!(error, AgentValidationError::EmptyPath { field: "raw_path" });
    }

    #[test]
    fn validation_checks_optional_decline_thesis_path_when_present() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.stance = Some("decline".to_string());
        raw.thesis_path = Some("theses/missing.md".to_string());

        let error = raw.validate(wiki.path()).expect_err("missing thesis file");
        assert_eq!(
            error,
            AgentValidationError::MissingFile {
                field: "thesis_path",
                path: "theses/missing.md".to_string()
            }
        );
    }

    #[test]
    fn validation_rejects_paths_outside_the_wiki() {
        let wiki = wiki_with_files();
        let outside_dir = tempfile::TempDir::new().expect("outside temp dir");
        let outside = outside_dir.path().join("outside.md");
        std::fs::write(&outside, "# Outside").expect("outside file");

        let mut absolute = valid_raw();
        absolute.raw_path = Some(outside.display().to_string());
        assert!(matches!(
            absolute.validate(wiki.path()),
            Err(AgentValidationError::OutsideWikiRoot {
                field: "raw_path",
                ..
            })
        ));

        let mut parent = valid_raw();
        parent.raw_path = Some("../outside.md".to_string());
        assert!(matches!(
            parent.validate(wiki.path()),
            Err(AgentValidationError::OutsideWikiRoot {
                field: "raw_path",
                ..
            })
        ));
    }
}
