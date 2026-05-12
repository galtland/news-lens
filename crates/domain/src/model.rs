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

/// Raw JSON shape printed by the agent before contract validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawAgentReturn {
    pub stance: Option<String>,
    pub raw_path: Option<String>,
    pub raw_slug: Option<String>,
    pub thesis_path: Option<String>,
    pub thesis_slug: Option<String>,
    pub one_liner: Option<String>,
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
    pub one_liner: Option<String>,
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

        let mut one_liner = self.one_liner.map(|value| normalize_one_liner(&value, 240));

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
            let required_one_liner = one_liner
                .take()
                .ok_or(AgentValidationError::MissingField("one_liner"))?;
            if required_one_liner.is_empty() {
                return Err(AgentValidationError::BlankOneLiner);
            }

            return Ok(AgentReturn {
                stance,
                raw_path,
                raw_slug: self.raw_slug,
                thesis_path: Some(thesis_path),
                thesis_slug: Some(thesis_slug),
                one_liner: Some(required_one_liner),
            });
        }

        let thesis_path = self
            .thesis_path
            .map(|path| normalize_existing_wiki_file(wiki_root, "thesis_path", &path))
            .transpose()?;

        Ok(AgentReturn {
            stance,
            raw_path,
            raw_slug: self.raw_slug,
            thesis_path,
            thesis_slug: self.thesis_slug,
            one_liner,
        })
    }
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
    #[error("one_liner is blank")]
    BlankOneLiner,
    #[error("thesis_slug is blank")]
    BlankThesisSlug,
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

    let wiki_relative_path = path.strip_prefix("wiki").unwrap_or(path);
    let canonical = wiki_root
        .join(wiki_relative_path)
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

fn normalize_one_liner(value: &str, max_len: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_len {
        return value.to_string();
    }

    let reserve = 3;
    let limit = max_len.saturating_sub(reserve);
    let prefix = value.chars().take(limit).collect::<String>();
    let candidate = prefix
        .rfind(char::is_whitespace)
        .filter(|idx| *idx > 0)
        .unwrap_or(prefix.len());
    format!("{}...", prefix[..candidate].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiki_with_files() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("raw/news")).expect("raw dir");
        std::fs::create_dir_all(dir.path().join("theses")).expect("theses dir");
        std::fs::write(dir.path().join("raw/news/item.md"), "# News").expect("raw file");
        std::fs::write(dir.path().join("theses/item.md"), "# Thesis").expect("thesis file");
        dir
    }

    fn valid_raw() -> RawAgentReturn {
        RawAgentReturn {
            stance: Some("critique".to_string()),
            raw_path: Some("raw/news/item.md".to_string()),
            raw_slug: Some("item".to_string()),
            thesis_path: Some("theses/item.md".to_string()),
            thesis_slug: Some("item".to_string()),
            one_liner: Some("A concise line.".to_string()),
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
        raw.one_liner = None;

        let output = raw.validate(wiki.path()).expect("failed stance");
        assert_eq!(output.stance, Stance::Failed);
        assert!(output.thesis_path.is_none());
        assert!(output.thesis_slug.is_none());
        assert!(output.one_liner.is_none());
    }

    #[test]
    fn validation_truncates_long_one_liner() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.one_liner = Some("word ".repeat(80));

        let output = raw.validate(wiki.path()).expect("valid with truncation");
        assert!(output.one_liner.expect("one_liner").chars().count() <= 240);
    }

    #[test]
    fn validation_truncates_multibyte_one_liner_by_character_count() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.one_liner = Some("é".repeat(260));

        let output = raw.validate(wiki.path()).expect("valid with truncation");
        let one_liner = output.one_liner.expect("one_liner");
        assert!(one_liner.chars().count() <= 240);
        assert!(one_liner.ends_with("..."));
    }

    #[test]
    fn validation_rejects_blank_one_liner_on_non_decline_stance() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.one_liner = Some(" \n\t ".to_string());

        let error = raw.validate(wiki.path()).expect_err("blank one_liner");
        assert_eq!(error, AgentValidationError::BlankOneLiner);
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
    fn validation_trims_one_liner_before_publishing() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.one_liner = Some("  A concise line. \n".to_string());

        let output = raw.validate(wiki.path()).expect("valid one_liner");
        assert_eq!(output.one_liner.as_deref(), Some("A concise line."));
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
    fn validation_normalizes_wiki_prefixed_paths() {
        let wiki = wiki_with_files();
        let mut raw = valid_raw();
        raw.raw_path = Some("wiki/raw/news/item.md".to_string());
        raw.thesis_path = Some("wiki/theses/item.md".to_string());

        let output = raw
            .validate(wiki.path())
            .expect("valid wiki-prefixed paths");

        assert_eq!(output.raw_path, "raw/news/item.md");
        assert_eq!(output.thesis_path.as_deref(), Some("theses/item.md"));
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
