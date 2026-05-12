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
}

impl SubprocessHarness {
    pub fn new(config: HarnessConfig) -> Self {
        Self { config }
    }

    pub fn command(&self) -> &str {
        &self.config.command
    }

    pub fn prompt_template(&self) -> &PathBuf {
        &self.config.prompt_template
    }

    async fn render_prompt(&self, ctx: &PostContext) -> Result<String, HarnessError> {
        let template = tokio::fs::read_to_string(&self.config.prompt_template)
            .await
            .map_err(|error| HarnessError::Io(error.to_string()))?;

        let post_json = serde_json::to_string_pretty(&ctx.post)
            .map_err(|error| HarnessError::InvalidResponse(error.to_string()))?;
        let created_at = ctx
            .post
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| HarnessError::InvalidResponse(error.to_string()))?;
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

        Ok(render_template(&template, &substitutions))
    }
}

fn render_template(template: &str, substitutions: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let token_start = &rest[start..];
        let Some(end) = token_start.find("}}") else {
            rendered.push_str(token_start);
            return rendered;
        };

        let token = &token_start[..end + 2];
        if let Some((_, replacement)) = substitutions
            .iter()
            .find(|(placeholder, _)| *placeholder == token)
        {
            rendered.push_str(replacement);
        } else {
            rendered.push_str(token);
        }
        rest = &token_start[end + 2..];
    }

    rendered.push_str(rest);
    rendered
}

#[async_trait]
impl Harness for SubprocessHarness {
    async fn process_post(&self, ctx: PostContext) -> Result<AgentReturn, HarnessError> {
        let prompt = self.render_prompt(&ctx).await?;

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

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|error| HarnessError::Io(error.to_string()))?;
        }

        let output = timeout(
            Duration::from_secs(self.config.timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| HarnessError::Timeout {
            timeout_secs: self.config.timeout_secs,
        })?
        .map_err(|error| HarnessError::Io(error.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !output.status.success() {
            return Err(HarnessError::Exit {
                status: output.status.to_string(),
                stderr,
            });
        }

        let json_line = stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .ok_or_else(|| HarnessError::InvalidResponse("stdout was empty".to_string()))?;

        let raw: RawAgentReturn = serde_json::from_str(json_line)
            .map_err(|error| HarnessError::InvalidResponse(error.to_string()))?;

        raw.validate(&ctx.wiki_path)
            .map_err(|error| HarnessError::Validation(error.to_string()))
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
        });

        let result = harness
            .process_post(make_context(wiki, dir.path().join("lens.md")))
            .await
            .expect("harness result");

        assert_eq!(result.stance, Stance::Critique);
        assert_eq!(result.thesis_slug.as_deref(), Some("item"));
    }

    #[test]
    fn render_template_does_not_rescan_substituted_values() {
        let rendered = render_template(
            "Post: {{POST_TEXT}}\nWiki: {{WIKI_PATH}}\n",
            &[
                ("{{POST_TEXT}}", "literal {{WIKI_PATH}}"),
                ("{{WIKI_PATH}}", "/tmp/wiki"),
            ],
        );

        assert_eq!(rendered, "Post: literal {{WIKI_PATH}}\nWiki: /tmp/wiki\n");
    }
}
