use anyhow::{Context, Result, bail};
use news_lens_adapters::{
    harness::{HarnessConfig as AdapterHarnessConfig, SubprocessHarness},
    lens::load_lens,
    nostr::NostrPublisher,
    outbox::{OutboxPublisher, OutboxWriter},
    x::{StubPostSource, XPostSource, XPublisher},
};
use news_lens_domain::{Lens, PostSource, Publisher, XPublishMode};
use secrecy::{ExposeSecret, SecretString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::AppConfig;

pub fn load_configured_lens(config: &AppConfig) -> Result<Lens> {
    let lens = load_lens(&config.lens.path)
        .with_context(|| format!("Failed to load lens: {}", config.lens.path.display()))?;
    if lens.id != config.lens.id {
        bail!(
            "Configured lens id '{}' does not match lens frontmatter id '{}'",
            config.lens.id,
            lens.id
        );
    }
    Ok(lens)
}

pub fn build_harness(config: &AppConfig) -> Result<SubprocessHarness> {
    SubprocessHarness::new(AdapterHarnessConfig {
        command: config.harness.command.clone(),
        args: config.harness.args.clone(),
        prompt_template: config.harness.prompt_template.clone(),
        timeout_secs: config.harness.timeout_secs,
    })
    .context("Failed to initialize harness")
}

pub fn build_post_source(config: &AppConfig) -> Result<Arc<dyn PostSource>> {
    if config.watch.accounts.is_empty() {
        return Ok(Arc::new(StubPostSource::empty()));
    }

    Ok(Arc::new(build_x_post_source(config)?))
}

pub fn build_x_post_source(config: &AppConfig) -> Result<XPostSource> {
    let bearer_token = load_api_key(&config.x.read.bearer_token_env, "x_read")?;
    Ok(XPostSource::with_page_cap(
        bearer_token,
        config.x.read.max_pages,
    ))
}

pub async fn build_publishers(
    config: &AppConfig,
    dry_run: bool,
    require_approval: bool,
    outbox_path: Option<PathBuf>,
) -> Result<(Arc<dyn Publisher>, Arc<dyn Publisher>)> {
    if require_approval {
        if !config.x.write.enabled && !config.nostr.enabled {
            bail!(
                "--require-approval requires at least one enabled publisher in [x.write] or [nostr]"
            );
        }

        let outbox_path = outbox_path.unwrap_or_else(default_outbox_path);
        let writer = OutboxWriter::new(outbox_path.clone())
            .await
            .context("Failed to initialize outbox writer")?;

        tracing::info!(outbox = %outbox_path.display(), "Writing approvals to outbox");

        let x_publisher: Arc<dyn Publisher> = if config.x.write.enabled {
            let x_mode = parse_x_publish_mode(&config.x.write.mode)?;
            Arc::new(OutboxPublisher::new_x(
                writer.clone(),
                x_mode,
                config.x.write.max_chars,
            ))
        } else {
            Arc::new(XPublisher::disabled())
        };

        let nostr_publisher: Arc<dyn Publisher> = if config.nostr.enabled {
            Arc::new(OutboxPublisher::new(writer, "nostr"))
        } else {
            Arc::new(NostrPublisher::disabled())
        };

        return Ok((x_publisher, nostr_publisher));
    }

    let x_publisher: Arc<dyn Publisher> = Arc::new(build_x_publisher(config, dry_run)?);
    let nostr_publisher: Arc<dyn Publisher> = Arc::new(build_nostr_publisher(config, dry_run)?);
    Ok((x_publisher, nostr_publisher))
}

pub fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

pub fn load_api_key(env_var: &str, provider: &str) -> Result<SecretString> {
    if env_var.trim().is_empty() {
        bail!("No API key env var configured for provider {}", provider);
    }

    let key = std::env::var(env_var).with_context(|| {
        format!(
            "Missing API key env var {} for provider {}",
            env_var, provider
        )
    })?;

    if key.trim().is_empty() {
        bail!(
            "API key env var {} is empty for provider {}",
            env_var,
            provider
        );
    }

    Ok(SecretString::new(key.into()))
}

fn build_x_publisher(config: &AppConfig, dry_run: bool) -> Result<XPublisher> {
    if dry_run || !config.x.write.enabled {
        return Ok(XPublisher::disabled());
    }

    let mode = parse_x_publish_mode(&config.x.write.mode)?;
    let user_token = load_api_key(&config.x.write.oauth2_user_token_env, "x_write")?;
    Ok(XPublisher::new(user_token, mode, config.x.write.max_chars))
}

fn build_nostr_publisher(config: &AppConfig, dry_run: bool) -> Result<NostrPublisher> {
    if dry_run || !config.nostr.enabled {
        return Ok(NostrPublisher::disabled());
    }

    if config.nostr.relays.is_empty() {
        bail!("Nostr enabled but no relays configured");
    }

    let secret_key = load_api_key(&config.nostr.secret_key_env, "nostr")?;
    NostrPublisher::new(secret_key.expose_secret(), config.nostr.relays.clone())
}

fn parse_x_publish_mode(mode: &str) -> Result<XPublishMode> {
    match mode.trim() {
        "reply" => Ok(XPublishMode::Reply),
        "quote" => Ok(XPublishMode::Quote),
        "new_post" => Ok(XPublishMode::NewPost),
        other => bail!("Invalid X publish mode: {}", other),
    }
}

fn default_outbox_path() -> PathBuf {
    PathBuf::from("./outbox.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_publishers_ignores_invalid_x_mode_when_x_disabled_in_dry_run() {
        let mut config = AppConfig::default();
        config.x.write.enabled = false;
        config.x.write.mode = "stale-mode".to_string();

        let (x_publisher, nostr_publisher) = build_publishers(&config, true, false, None)
            .await
            .expect("publishers");

        assert!(!x_publisher.is_enabled());
        assert!(!nostr_publisher.is_enabled());
    }

    #[tokio::test]
    async fn build_publishers_ignores_invalid_x_mode_for_nostr_only_approval() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let outbox_path = dir.path().join("outbox.jsonl");
        let mut config = AppConfig::default();
        config.x.write.enabled = false;
        config.x.write.mode = "stale-mode".to_string();
        config.nostr.enabled = true;

        let (x_publisher, nostr_publisher) =
            build_publishers(&config, false, true, Some(outbox_path))
                .await
                .expect("publishers");

        assert!(!x_publisher.is_enabled());
        assert!(nostr_publisher.is_enabled());
    }

    #[tokio::test]
    async fn build_publishers_rejects_invalid_x_mode_when_x_approval_is_enabled() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let outbox_path = dir.path().join("outbox.jsonl");
        let mut config = AppConfig::default();
        config.x.write.enabled = true;
        config.x.write.mode = "stale-mode".to_string();

        let error = match build_publishers(&config, false, true, Some(outbox_path)).await {
            Ok(_) => panic!("invalid x mode should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("Invalid X publish mode"));
    }
}
