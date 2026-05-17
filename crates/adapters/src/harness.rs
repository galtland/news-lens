//! Subprocess harness adapter.

use async_trait::async_trait;
use news_lens_domain::{AgentReturn, Harness, HarnessError, PostContext, RawAgentReturn};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const ALLOWED_TEMPLATE_TOKENS: &[&str] = &[
    "{{POST_ID}}",
    "{{POST_TEXT}}",
    "{{POST_AUTHOR}}",
    "{{POST_URL}}",
    "{{POST_CREATED_AT}}",
    "{{POST_JSON}}",
    "{{WIKI_PATH}}",
    "{{MANIFEST_PATH}}",
    "{{LENS_PATH}}",
    "{{LENS_ID}}",
    "{{LENS_VOICE}}",
    "{{LENS_REGISTER}}",
    "{{LENS_CONTENT}}",
    "{{CANDIDATE_SLUG}}",
    "{{PUBLIC_BASE_URL}}",
];

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub prompt_template: PathBuf,
    pub timeout_secs: u64,
    /// Public base URL used by the agent to construct sources-reply URLs in the
    /// X thread (e.g. `https://douglaz.github.io`). Substituted into the prompt
    /// as `{{PUBLIC_BASE_URL}}`. Empty string when not configured.
    pub public_base_url: String,
}

#[derive(Debug, Clone)]
pub struct SubprocessHarness {
    config: HarnessConfig,
    prompt_template: String,
}

impl SubprocessHarness {
    pub fn new(config: HarnessConfig) -> Result<Self, HarnessError> {
        let prompt_template = std::fs::read_to_string(&config.prompt_template)
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        validate_template(&prompt_template)?;
        Ok(Self {
            config,
            prompt_template,
        })
    }

    pub fn command(&self) -> &str {
        &self.config.command
    }

    pub fn prompt_template(&self) -> &PathBuf {
        &self.config.prompt_template
    }

    fn render_prompt(
        &self,
        ctx: &PostContext,
        manifest_path: &Path,
    ) -> Result<String, HarnessError> {
        let post_json = serde_json::to_string_pretty(&ctx.post)
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        let created_at = ctx
            .post
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        let wiki_path = ctx.wiki_path.display().to_string();
        let manifest_path = manifest_path.display().to_string();
        let lens_path = ctx.lens.path.display().to_string();

        let substitutions = [
            ("{{POST_ID}}", ctx.post.id.as_str()),
            ("{{POST_TEXT}}", ctx.post.text.as_str()),
            ("{{POST_AUTHOR}}", ctx.post.author.as_str()),
            ("{{POST_URL}}", ctx.post.url.as_str()),
            ("{{POST_CREATED_AT}}", created_at.as_str()),
            ("{{POST_JSON}}", post_json.as_str()),
            ("{{WIKI_PATH}}", wiki_path.as_str()),
            ("{{MANIFEST_PATH}}", manifest_path.as_str()),
            ("{{LENS_PATH}}", lens_path.as_str()),
            ("{{LENS_ID}}", ctx.lens.id.as_str()),
            ("{{LENS_VOICE}}", ctx.lens.voice.as_deref().unwrap_or("")),
            (
                "{{LENS_REGISTER}}",
                ctx.lens.register.as_deref().unwrap_or(""),
            ),
            ("{{LENS_CONTENT}}", ctx.lens.content.as_str()),
            ("{{CANDIDATE_SLUG}}", ctx.candidate_slug.as_str()),
            ("{{PUBLIC_BASE_URL}}", self.config.public_base_url.as_str()),
        ];

        render_template(&self.prompt_template, &substitutions)
    }

    fn manifest_path(&self, wiki_path: &Path, post_id: &str) -> Result<PathBuf, HarnessError> {
        manifest_path(wiki_path, post_id)
    }
}

fn validate_template(template: &str) -> Result<(), HarnessError> {
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        let token_start = &rest[start..];
        let Some(end) = token_start.find("}}") else {
            return Err(unterminated_template_token(token_start));
        };

        let token = &token_start[..end + 2];
        if !ALLOWED_TEMPLATE_TOKENS.contains(&token) {
            return Err(unknown_template_token(token));
        }
        rest = &token_start[end + 2..];
    }

    Ok(())
}

fn render_template(template: &str, substitutions: &[(&str, &str)]) -> Result<String, HarnessError> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let token_start = &rest[start..];
        let Some(end) = token_start.find("}}") else {
            return Err(unterminated_template_token(token_start));
        };

        let token = &token_start[..end + 2];
        if let Some((_, replacement)) = substitutions
            .iter()
            .find(|(placeholder, _)| *placeholder == token)
        {
            rendered.push_str(replacement);
        } else {
            return Err(unknown_template_token(token));
        }
        rest = &token_start[end + 2..];
    }

    rendered.push_str(rest);
    Ok(rendered)
}

fn unknown_template_token(token: &str) -> HarnessError {
    let token_name = template_token_name(token);
    HarnessError::InvalidTemplate(format!("unknown template token: {}", token_name))
}

fn unterminated_template_token(token: &str) -> HarnessError {
    let token_name = template_token_name(token);
    HarnessError::InvalidTemplate(format!("unterminated template token: {}", token_name))
}

fn template_token_name(token: &str) -> &str {
    token.trim_start_matches("{{").trim_end_matches("}}").trim()
}

#[async_trait]
impl Harness for SubprocessHarness {
    async fn process_post(&self, ctx: PostContext) -> Result<AgentReturn, HarnessError> {
        let manifest_path = self.manifest_path(&ctx.wiki_path, &ctx.post.id)?;
        let prompt = self.render_prompt(&ctx, &manifest_path)?;
        let Some(manifest_parent) = manifest_path.parent() else {
            return Err(HarnessError::Io(format!(
                "manifest path has no parent: {}",
                manifest_path.display()
            )));
        };
        ensure_wiki_path_exists(&absolute_path(&ctx.wiki_path)?).await?;
        tokio::fs::create_dir_all(manifest_parent)
            .await
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        remove_stale_manifest(&manifest_path).await?;
        let manifest_path_env = manifest_path.display().to_string();

        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .env("NEWS_LENS_MANIFEST_PATH", &manifest_path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|error| HarnessError::Io(error.to_string()))?;

        let stdin = child.stdin.take();
        let (output, stdin_result) =
            timeout(Duration::from_secs(self.config.timeout_secs), async move {
                let write_stdin = async move {
                    if let Some(mut stdin) = stdin {
                        stdin.write_all(prompt.as_bytes()).await?;
                        stdin.shutdown().await?;
                    }
                    Ok::<(), std::io::Error>(())
                };

                let (stdin_result, output_result) =
                    tokio::join!(write_stdin, child.wait_with_output());
                output_result.map(|output| (output, stdin_result))
            })
            .await
            .map_err(|_| HarnessError::Timeout {
                timeout_secs: self.config.timeout_secs,
            })?
            .map_err(|error| HarnessError::Io(error.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr_text.trim().to_string();

        if !output.status.success() {
            let parsed = read_raw_agent_return_manifest(&manifest_path, &stderr_text).await;
            let (raw, parse_error) = match parsed {
                Ok(raw) => (Some(Box::new(raw)), None),
                Err(error) => (None, Some(error.to_string())),
            };

            return Err(HarnessError::Exit {
                status: output.status.to_string(),
                stderr,
                stdout_tail: output_tail(&stdout),
                parse_error,
                raw,
            });
        }

        let raw = read_raw_agent_return_manifest(&manifest_path, &stderr_text).await?;
        if let Err(error) = stdin_result {
            tracing::warn!(
                error = %error,
                stdout_tail = %output_tail(&stdout),
                stderr = %stderr,
                "Harness stdin write failed after valid response"
            );
        }

        raw.clone()
            .validate(&ctx.wiki_path)
            .map_err(|error| HarnessError::Validation {
                message: error.to_string(),
                raw: Box::new(raw),
            })
    }
}

fn manifest_path(wiki_path: &Path, post_id: &str) -> Result<PathBuf, HarnessError> {
    Ok(absolute_path(wiki_path)?
        .join(".news-lens")
        .join(format!("{}.json", sanitize_manifest_post_id(post_id))))
}

fn absolute_path(path: &Path) -> Result<PathBuf, HarnessError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| HarnessError::Io(error.to_string()))
}

fn sanitize_manifest_post_id(post_id: &str) -> String {
    let sanitized: String = post_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "post".to_string()
    } else {
        sanitized
    }
}

async fn ensure_wiki_path_exists(wiki_path: &Path) -> Result<(), HarnessError> {
    let metadata = tokio::fs::metadata(wiki_path).await.map_err(|error| {
        HarnessError::Io(format!(
            "could not access wiki path {}: {}",
            wiki_path.display(),
            error
        ))
    })?;

    if metadata.is_dir() {
        Ok(())
    } else {
        Err(HarnessError::Io(format!(
            "wiki path is not a directory: {}",
            wiki_path.display()
        )))
    }
}

async fn remove_stale_manifest(manifest_path: &Path) -> Result<(), HarnessError> {
    match tokio::fs::remove_file(manifest_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(HarnessError::Io(format!(
            "could not remove stale manifest {}: {}",
            manifest_path.display(),
            error
        ))),
    }
}

async fn read_raw_agent_return_manifest(
    manifest_path: &Path,
    stderr: &str,
) -> Result<RawAgentReturn, HarnessError> {
    let bytes = tokio::fs::read(manifest_path).await.map_err(|error| {
        HarnessError::InvalidResponse(format!(
            "could not read manifest {}: {}; stderr tail: {}",
            manifest_path.display(),
            error,
            output_tail(stderr)
        ))
    })?;

    serde_json::from_slice::<RawAgentReturn>(&bytes).map_err(|error| {
        HarnessError::InvalidResponse(format!(
            "manifest {} contained malformed JSON: {}; stderr tail: {}",
            manifest_path.display(),
            error,
            output_tail(stderr)
        ))
    })
}

fn output_tail(output: &str) -> String {
    let mut lines = output
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .collect::<Vec<_>>();
    lines.reverse();

    let tail = lines.join("\n");
    if tail.len() <= 1_000 {
        tail
    } else {
        let start = tail
            .char_indices()
            .find_map(|(idx, _)| (idx >= tail.len() - 1_000).then_some(idx))
            .unwrap_or(0);
        format!("...{}", &tail[start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use news_lens_domain::{Lens, SourcePost, Stance};
    use std::os::unix::fs::PermissionsExt;
    use time::OffsetDateTime;

    fn make_post() -> SourcePost {
        SourcePost {
            id: "post-1".to_string(),
            text: "Test news item".to_string(),
            author: "tester".to_string(),
            url: "https://example.com/post-1".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            is_repost: false,
            is_reply: false,
            reply_to_id: None,
        }
    }

    fn make_context(wiki_path: PathBuf, lens_path: PathBuf) -> PostContext {
        PostContext {
            post: make_post(),
            wiki_path,
            lens: Lens {
                id: "test-lens".to_string(),
                voice: Some("terse".to_string()),
                register: Some("written".to_string()),
                path: lens_path,
                content: "Lens body".to_string(),
            },
            candidate_slug: "1970-01-01-test-news-item".to_string(),
        }
    }

    fn wiki_with_files(root: &Path) -> PathBuf {
        let wiki = root.join("wiki");
        std::fs::create_dir_all(wiki.join("raw/news")).expect("raw dir");
        std::fs::create_dir_all(wiki.join("theses")).expect("theses dir");
        std::fs::create_dir_all(wiki.join("wiki/theses")).expect("fixture thesis dir");
        std::fs::write(wiki.join("raw/news/item.md"), "# News").expect("raw file");
        std::fs::write(wiki.join("raw/news/test-news-item.md"), "# Fixture news")
            .expect("fixture raw file");
        std::fs::write(wiki.join("theses/item.md"), "# Thesis").expect("thesis file");
        std::fs::write(wiki.join("wiki/theses/test-thesis.md"), "# Fixture thesis")
            .expect("fixture thesis file");
        wiki
    }

    fn write_script(script: &Path, body: &str) {
        std::fs::write(script, body).expect("script");
        let mut permissions = std::fs::metadata(script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("chmod");
    }

    #[test]
    fn harness_rejects_missing_prompt_template_at_construction() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let error = SubprocessHarness::new(HarnessConfig {
            command: "true".to_string(),
            args: vec![],
            prompt_template: dir.path().join("missing.md"),
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect_err("missing prompt");

        assert!(matches!(error, HarnessError::Io(_)));
    }

    #[test]
    fn harness_rejects_unknown_prompt_token_at_construction() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "Post: {{POST_HEADLINE}}\n").expect("prompt");

        let error = SubprocessHarness::new(HarnessConfig {
            command: "true".to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect_err("invalid prompt");

        assert!(
            matches!(error, HarnessError::InvalidTemplate(message) if message.contains("POST_HEADLINE"))
        );
    }

    #[test]
    fn harness_rejects_unterminated_prompt_token_at_construction() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "Post: {{POST_TEXT\n").expect("prompt");

        let error = SubprocessHarness::new(HarnessConfig {
            command: "true".to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect_err("invalid prompt");

        assert!(
            matches!(error, HarnessError::InvalidTemplate(message) if message.contains("unterminated template token: POST_TEXT"))
        );
    }

    #[tokio::test]
    async fn harness_reads_manifest_written_by_fixture_script() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(
            &prompt_path,
            "Post: {{POST_TEXT}}\nWiki: {{WIKI_PATH}}\nManifest: {{MANIFEST_PATH}}\nLens: {{LENS_CONTENT}}\n",
        )
        .expect("prompt");
        let fixture_script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("fixtures/harness/stub-harness.sh");

        let harness = SubprocessHarness::new(HarnessConfig {
            command: "sh".to_string(),
            args: vec![fixture_script.display().to_string()],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let result = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect("harness result");

        assert_eq!(result.stance, Stance::Critique);
        assert_eq!(result.raw_path, "raw/news/test-news-item.md");
        assert_eq!(result.thesis_slug.as_deref(), Some("test-thesis"));
    }

    #[tokio::test]
    async fn harness_passes_absolute_manifest_path_for_relative_wiki_path() {
        let cwd = std::env::current_dir().expect("cwd");
        let dir = tempfile::TempDir::new_in(&cwd).expect("temp dir");
        let wiki = wiki_with_files(dir.path());
        let relative_wiki = wiki
            .strip_prefix(&cwd)
            .expect("wiki under cwd")
            .to_path_buf();
        assert!(!relative_wiki.is_absolute());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("absolute-manifest.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
case "$NEWS_LENS_MANIFEST_PATH" in
  /*) ;;
  *)
    echo "manifest path is relative: $NEWS_LENS_MANIFEST_PATH" >&2
    exit 9
    ;;
esac
cd /
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","thread":["Absolute path lead.","Sources: https://example.test/concepts/foo"]}
EOF
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let result = harness
            .process_post(make_context(relative_wiki, dir.path().join("lens.md")))
            .await
            .expect("harness result");

        assert_eq!(result.raw_path, "raw/news/item.md");
    }

    #[tokio::test]
    async fn harness_rejects_missing_manifest_after_success() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("missing-manifest.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo "human stdout"
echo "missing manifest sentinel" >&2
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let manifest_path = manifest_path(&wiki, "post-1").expect("manifest path");
        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("missing manifest");

        assert!(matches!(error, HarnessError::InvalidResponse(message)
                if message.contains(&manifest_path.display().to_string())
                    && message.contains("could not read manifest")
                    && message.contains("missing manifest sentinel")));
    }

    #[tokio::test]
    async fn harness_rejects_malformed_manifest_json() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("malformed-manifest.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo "malformed manifest sentinel" >&2
printf '{not json\n' >"$NEWS_LENS_MANIFEST_PATH"
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let manifest_path = manifest_path(&wiki, "post-1").expect("manifest path");
        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("malformed manifest");

        assert!(matches!(error, HarnessError::InvalidResponse(message)
                if message.contains(&manifest_path.display().to_string())
                    && message.contains("malformed JSON")
                    && message.contains("malformed manifest sentinel")));
    }

    #[tokio::test]
    async fn harness_rejects_missing_wiki_root_without_creating_it() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let missing_wiki = dir.path().join("missing-wiki");
        let spawned_marker = dir.path().join("spawned");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("should-not-run.sh");
        write_script(
            &script,
            &format!(
                r#"#!/bin/sh
cat >/dev/null
touch "{}"
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{{"stance":"decline","raw_path":"raw/news/item.md","raw_slug":"item"}}
EOF
"#,
                spawned_marker.display()
            ),
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let error = harness
            .process_post(make_context(
                missing_wiki.clone(),
                dir.path().join("lens.md"),
            ))
            .await
            .expect_err("missing wiki");

        assert!(matches!(error, HarnessError::Io(message)
                if message.contains(&missing_wiki.display().to_string())
                    && message.contains("could not access wiki path")));
        assert!(!missing_wiki.exists());
        assert!(!spawned_marker.exists());
    }

    #[tokio::test]
    async fn harness_rejects_stale_manifest_removal_failure_before_spawning() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());
        let spawned_marker = dir.path().join("spawned");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("should-not-run.sh");
        write_script(
            &script,
            &format!(
                r#"#!/bin/sh
cat >/dev/null
touch "{}"
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{{"stance":"decline","raw_path":"raw/news/item.md","raw_slug":"item"}}
EOF
"#,
                spawned_marker.display()
            ),
        );

        let stale_manifest = manifest_path(&wiki, "post-1").expect("manifest path");
        std::fs::create_dir_all(&stale_manifest).expect("stale manifest dir");

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("stale manifest removal failure");

        assert!(matches!(error, HarnessError::Io(message)
                if message.contains(&stale_manifest.display().to_string())
                    && message.contains("could not remove stale manifest")));
        assert!(!spawned_marker.exists());
    }

    #[tokio::test]
    async fn harness_removes_stale_manifest_before_subprocess_executes() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("fresh-manifest.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
if [ -e "$NEWS_LENS_MANIFEST_PATH" ]; then
  echo "stale manifest was not removed" >&2
  exit 9
fi
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","thread":["Fresh lead.","Sources: https://example.test/concepts/foo"]}
EOF
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");
        let stale_manifest = manifest_path(&wiki, "post-1").expect("manifest path");
        std::fs::create_dir_all(stale_manifest.parent().expect("manifest parent"))
            .expect("manifest dir");
        std::fs::write(
            &stale_manifest,
            r#"{"stance":"decline","raw_path":"raw/news/stale.md"}"#,
        )
        .expect("stale manifest");

        let result = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect("harness result");
        let manifest_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&stale_manifest).expect("manifest"))
                .expect("manifest json");

        assert_eq!(result.thesis_slug.as_deref(), Some("item"));
        assert_eq!(manifest_value["raw_path"], "raw/news/item.md");
    }

    #[tokio::test]
    async fn harness_sanitizes_manifest_path_post_id() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("sanitized-manifest.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","thread":["Sanitized lead.","Sources: https://example.test/concepts/foo"]}
EOF
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");
        let mut ctx = make_context(wiki.clone(), dir.path().join("lens.md"));
        ctx.post.id = "post / one two".to_string();
        let expected_manifest = manifest_path(&wiki, "post / one two").expect("manifest path");

        harness.process_post(ctx).await.expect("harness result");

        let file_name = expected_manifest
            .file_name()
            .and_then(|name| name.to_str())
            .expect("manifest file name");
        assert_eq!(file_name, "post___one_two.json");
        assert!(expected_manifest.exists());
    }

    #[test]
    fn manifest_filename_uses_sanitized_post_id() {
        let wiki = Path::new("/tmp/wiki");
        let slash = manifest_path(wiki, "foo/123").expect("slash manifest");
        let space = manifest_path(wiki, "foo 123").expect("space manifest");
        let empty = manifest_path(wiki, "").expect("empty manifest");

        let slash_name = slash.file_name().and_then(|name| name.to_str()).unwrap();
        let space_name = space.file_name().and_then(|name| name.to_str()).unwrap();
        let empty_name = empty.file_name().and_then(|name| name.to_str()).unwrap();

        assert_eq!(slash_name, "foo_123.json");
        assert_eq!(space_name, "foo_123.json");
        assert_eq!(empty_name, "post.json");
    }

    #[tokio::test]
    async fn harness_timeout_covers_stdin_write() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(&wiki).expect("wiki dir");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{LENS_CONTENT}}").expect("prompt");

        let script = dir.path().join("sleeping-harness.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
sleep 5
"#,
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");

        let mut ctx = make_context(wiki, dir.path().join("lens.md"));
        ctx.lens.content = "x".repeat(2_000_000);

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 1,
            public_base_url: String::new(),
        })
        .expect("harness");

        let error = harness.process_post(ctx).await.expect_err("timeout");
        assert!(matches!(error, HarnessError::Timeout { timeout_secs: 1 }));
    }

    #[tokio::test]
    async fn harness_accepts_valid_json_when_child_closes_stdin_early() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{LENS_CONTENT}}").expect("prompt");

        let script = dir.path().join("early-close-harness.sh");
        write_script(
            &script,
            r#"#!/bin/sh
exec 0<&-
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","thread":["Lead analytic claim.","Sources: https://example.test/concepts/foo"]}
EOF
"#,
        );

        let mut ctx = make_context(wiki, dir.path().join("lens.md"));
        ctx.lens.content = "x".repeat(2_000_000);

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let result = harness.process_post(ctx).await.expect("harness result");

        assert_eq!(result.stance, Stance::Critique);
    }

    #[tokio::test]
    async fn harness_exit_preserves_captured_output_and_manifest_on_nonzero_exit() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = wiki_with_files(dir.path());

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("failing-harness.sh");
        write_script(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo "diagnostic line"
cat >"$NEWS_LENS_MANIFEST_PATH" <<'EOF'
{"stance":"decline","raw_path":"raw/news/item.md","raw_slug":"item"}
EOF
echo "cleanup failed" >&2
exit 7
"#,
        );

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
            public_base_url: String::new(),
        })
        .expect("harness");

        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("non-zero exit");

        match error {
            HarnessError::Exit {
                status,
                stderr,
                stdout_tail,
                parse_error,
                raw,
            } => {
                assert!(status.contains('7'));
                assert!(stderr.contains("cleanup failed"));
                assert!(stdout_tail.contains("diagnostic line"));
                assert!(parse_error.is_none());
                let raw = raw.expect("manifest parsed on exit");
                assert_eq!(raw.raw_path.as_deref(), Some("raw/news/item.md"));
                assert_eq!(raw.raw_slug.as_deref(), Some("item"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn render_template_does_not_rescan_substituted_values() {
        let rendered = render_template(
            "Post: {{POST_TEXT}}\nWiki: {{WIKI_PATH}}\n",
            &[
                ("{{POST_TEXT}}", "literal {{WIKI_PATH}}"),
                ("{{WIKI_PATH}}", "/tmp/wiki"),
            ],
        )
        .expect("rendered");

        assert_eq!(rendered, "Post: literal {{WIKI_PATH}}\nWiki: /tmp/wiki\n");
    }

    #[test]
    fn render_template_rejects_unknown_token() {
        let error = render_template("Post: {{POST_HEADLINE}}\n", &[("{{POST_TEXT}}", "hello")])
            .expect_err("unknown token");

        assert!(
            matches!(error, HarnessError::InvalidTemplate(message) if message.contains("POST_HEADLINE"))
        );
    }

    #[test]
    fn render_template_rejects_unterminated_token() {
        let error = render_template("Post: {{POST_TEXT\n", &[("{{POST_TEXT}}", "hello")])
            .expect_err("unterminated token");

        assert!(
            matches!(error, HarnessError::InvalidTemplate(message) if message.contains("unterminated template token: POST_TEXT"))
        );
    }
}
