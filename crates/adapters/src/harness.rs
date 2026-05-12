//! Subprocess harness adapter.

use async_trait::async_trait;
use news_lens_domain::{AgentReturn, Harness, HarnessError, PostContext, RawAgentReturn};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub prompt_template: PathBuf,
    pub timeout_secs: u64,
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

    fn render_prompt(&self, ctx: &PostContext) -> Result<String, HarnessError> {
        let post_json = serde_json::to_string_pretty(&ctx.post)
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        let created_at = ctx
            .post
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        let wiki_path = ctx.wiki_path.display().to_string();
        let lens_path = ctx.lens.path.display().to_string();

        let substitutions = [
            ("{{POST_ID}}", ctx.post.id.as_str()),
            ("{{POST_TEXT}}", ctx.post.text.as_str()),
            ("{{POST_AUTHOR}}", ctx.post.author.as_str()),
            ("{{POST_URL}}", ctx.post.url.as_str()),
            ("{{POST_CREATED_AT}}", created_at.as_str()),
            ("{{POST_JSON}}", post_json.as_str()),
            ("{{WIKI_PATH}}", wiki_path.as_str()),
            ("{{LENS_PATH}}", lens_path.as_str()),
            ("{{LENS_ID}}", ctx.lens.id.as_str()),
            ("{{LENS_VOICE}}", ctx.lens.voice.as_deref().unwrap_or("")),
            (
                "{{LENS_REGISTER}}",
                ctx.lens.register.as_deref().unwrap_or(""),
            ),
            ("{{LENS_CONTENT}}", ctx.lens.content.as_str()),
            ("{{CANDIDATE_SLUG}}", ctx.candidate_slug.as_str()),
        ];

        render_template(&self.prompt_template, &substitutions)
    }
}

fn render_template(template: &str, substitutions: &[(&str, &str)]) -> Result<String, HarnessError> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let token_start = &rest[start..];
        let Some(end) = token_start.find("}}") else {
            rendered.push_str(token_start);
            return Ok(rendered);
        };

        let token = &token_start[..end + 2];
        if let Some((_, replacement)) = substitutions
            .iter()
            .find(|(placeholder, _)| *placeholder == token)
        {
            rendered.push_str(replacement);
        } else {
            let token_name = token.trim_start_matches("{{").trim_end_matches("}}").trim();
            return Err(HarnessError::InvalidResponse(format!(
                "unknown template token: {}",
                token_name
            )));
        }
        rest = &token_start[end + 2..];
    }

    rendered.push_str(rest);
    Ok(rendered)
}

#[async_trait]
impl Harness for SubprocessHarness {
    async fn process_post(&self, ctx: PostContext) -> Result<AgentReturn, HarnessError> {
        let prompt = self.render_prompt(&ctx)?;

        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
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
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let parsed = parse_raw_agent_return(&stdout);

        if !output.status.success() {
            let parse_error = parsed.as_ref().err().map(ToString::to_string);
            return Err(HarnessError::Exit {
                status: output.status.to_string(),
                stderr,
                stdout_tail: stdout_tail(&stdout),
                parse_error,
                raw: parsed.ok().map(Box::new),
            });
        }

        let raw = parsed?;
        if let Err(error) = stdin_result {
            tracing::warn!(
                error = %error,
                stdout_tail = %stdout_tail(&stdout),
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

fn parse_raw_agent_return(stdout: &str) -> Result<RawAgentReturn, HarnessError> {
    let mut saw_line = false;
    let tail = stdout_tail(stdout);

    for line in stdout.lines().rev() {
        let candidate = line.trim().trim_start_matches('\u{feff}').trim();
        if candidate.is_empty() {
            continue;
        }
        saw_line = true;

        let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) else {
            continue;
        };

        if value.get("stance").is_none() {
            continue;
        }

        return serde_json::from_value::<RawAgentReturn>(value).map_err(|error| {
            HarnessError::InvalidResponse(format!(
                "contract JSON line was malformed: {}; line: {}",
                error, candidate
            ))
        });
    }

    if !saw_line {
        return Err(HarnessError::InvalidResponse(
            "stdout was empty".to_string(),
        ));
    }

    Err(HarnessError::InvalidResponse(format!(
        "stdout did not contain contract JSON with a stance field; stdout tail: {}",
        tail
    )))
}

fn stdout_tail(stdout: &str) -> String {
    let mut lines = stdout
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

    #[test]
    fn harness_rejects_missing_prompt_template_at_construction() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let error = SubprocessHarness::new(HarnessConfig {
            command: "true".to_string(),
            args: vec![],
            prompt_template: dir.path().join("missing.md"),
            timeout_secs: 5,
        })
        .expect_err("missing prompt");

        assert!(matches!(error, HarnessError::Io(_)));
    }

    #[tokio::test]
    async fn harness_runs_stub_script_and_parses_final_json_line() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(wiki.join("raw/news")).expect("raw dir");
        std::fs::create_dir_all(wiki.join("theses")).expect("theses dir");
        std::fs::write(wiki.join("raw/news/item.md"), "# News").expect("raw file");
        std::fs::write(wiki.join("theses/item.md"), "# Thesis").expect("thesis file");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(
            &prompt_path,
            "Post: {{POST_TEXT}}\nWiki: {{WIKI_PATH}}\nLens: {{LENS_CONTENT}}\n",
        )
        .expect("prompt");

        let script = dir.path().join("stub-harness.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo "diagnostic line"
echo '{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","one_liner":"One line."}'
"#,
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
        })
        .expect("harness");

        let result = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect("harness result");

        assert_eq!(result.stance, Stance::Critique);
        assert_eq!(result.thesis_slug.as_deref(), Some("item"));
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
        })
        .expect("harness");

        let error = harness.process_post(ctx).await.expect_err("timeout");
        assert!(matches!(error, HarnessError::Timeout { timeout_secs: 1 }));
    }

    #[tokio::test]
    async fn harness_accepts_valid_json_when_child_closes_stdin_early() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(wiki.join("raw/news")).expect("raw dir");
        std::fs::create_dir_all(wiki.join("theses")).expect("theses dir");
        std::fs::write(wiki.join("raw/news/item.md"), "# News").expect("raw file");
        std::fs::write(wiki.join("theses/item.md"), "# Thesis").expect("thesis file");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{LENS_CONTENT}}").expect("prompt");

        let script = dir.path().join("early-close-harness.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
exec 0<&-
echo '{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","one_liner":"One line."}'
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
            timeout_secs: 5,
        })
        .expect("harness");

        let result = harness.process_post(ctx).await.expect("harness result");

        assert_eq!(result.stance, Stance::Critique);
    }

    #[tokio::test]
    async fn harness_preserves_contract_json_on_nonzero_exit() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(wiki.join("raw/news")).expect("raw dir");
        std::fs::write(wiki.join("raw/news/item.md"), "# News").expect("raw file");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("failing-harness.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo '{"stance":"decline","raw_path":"raw/news/item.md","raw_slug":"item"}'
echo "cleanup failed" >&2
exit 7
"#,
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
        })
        .expect("harness");

        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("non-zero exit");

        match error {
            HarnessError::Exit { status, raw, .. } => {
                assert!(status.contains('7'));
                let raw = raw.expect("contract JSON");
                assert_eq!(raw.stance.as_deref(), Some("decline"));
                assert_eq!(raw.raw_path.as_deref(), Some("raw/news/item.md"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn harness_exit_preserves_parse_error_when_contract_json_is_missing() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(wiki.join("raw/news")).expect("raw dir");

        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "{{POST_TEXT}}").expect("prompt");

        let script = dir.path().join("failing-no-contract.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
cat >/dev/null
echo "diagnostic line"
echo '{"trace_id":"abc"}'
echo "cleanup failed" >&2
exit 7
"#,
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");

        let harness = SubprocessHarness::new(HarnessConfig {
            command: script.display().to_string(),
            args: vec![],
            prompt_template: prompt_path,
            timeout_secs: 5,
        })
        .expect("harness");

        let error = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect_err("non-zero exit");

        match error {
            HarnessError::Exit {
                parse_error,
                stdout_tail,
                raw,
                ..
            } => {
                assert!(raw.is_none());
                assert!(stdout_tail.contains("diagnostic line"));
                assert!(
                    parse_error
                        .as_deref()
                        .is_some_and(|message| message.contains("stance field"))
                );
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
            matches!(error, HarnessError::InvalidResponse(message) if message.contains("POST_HEADLINE"))
        );
    }

    #[test]
    fn parse_raw_agent_return_uses_final_json_line() {
        let stdout = r#"
diagnostic line
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","one_liner":"One line."}
"#;

        let raw = parse_raw_agent_return(stdout).expect("raw JSON");

        assert_eq!(raw.stance.as_deref(), Some("critique"));
        assert_eq!(raw.thesis_slug.as_deref(), Some("item"));
    }

    #[test]
    fn parse_raw_agent_return_skips_non_contract_json_after_contract_json() {
        let stdout = r#"
{"stance":"critique","raw_path":"raw/news/item.md","raw_slug":"item","thesis_path":"theses/item.md","thesis_slug":"item","one_liner":"One line."}
{"trace_id":"abc"}
"#;

        let raw = parse_raw_agent_return(stdout).expect("raw JSON");

        assert_eq!(raw.stance.as_deref(), Some("critique"));
        assert_eq!(raw.thesis_slug.as_deref(), Some("item"));
    }

    #[test]
    fn parse_raw_agent_return_rejects_output_without_contract_json() {
        let stdout = r#"
{"trace_id":"abc"}
not json
"#;

        let error = parse_raw_agent_return(stdout).expect_err("missing contract line");

        assert!(
            matches!(error, HarnessError::InvalidResponse(message) if message.contains("stance field"))
        );
    }

    #[test]
    fn parse_raw_agent_return_rejects_malformed_contract_json_line() {
        let stdout = r#"
{"stance":123,"raw_path":"raw/news/item.md"}
"#;

        let error = parse_raw_agent_return(stdout).expect_err("malformed contract line");

        assert!(
            matches!(error, HarnessError::InvalidResponse(message) if message.contains("contract JSON line was malformed"))
        );
    }
}
