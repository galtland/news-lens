//! Configuration loading and management.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Prompt shipped with the binary for `config init` installations.
pub const SHIPPED_PROCESS_PROMPT: &str = include_str!("../../../prompts/process-post.md");

/// Top-level configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default)]
    pub wiki: WikiConfig,

    #[serde(default)]
    pub lens: LensConfig,

    #[serde(default)]
    pub harness: HarnessConfig,

    #[serde(default)]
    pub publish: PublishConfig,

    #[serde(default)]
    pub watch: WatchConfig,

    #[serde(default)]
    pub x: XConfig,

    #[serde(default)]
    pub nostr: NostrConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_state_db_path")]
    pub state_db_path: PathBuf,

    #[serde(default = "default_log_level")]
    pub log_level: String,

    #[serde(default = "default_true")]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiConfig {
    #[serde(default = "default_wiki_path")]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LensConfig {
    #[serde(default = "default_lens_path")]
    pub path: PathBuf,

    #[serde(default = "default_lens_id")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    #[serde(default = "default_harness_command")]
    pub command: String,

    #[serde(default = "default_harness_args")]
    pub args: Vec<String>,

    #[serde(default = "default_prompt_template")]
    pub prompt_template: PathBuf,

    #[serde(default = "default_harness_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishConfig {
    /// Base URL prefix for sources-reply links in the X thread (and equivalently
    /// for the lead Nostr note's source URL). Example: `https://douglaz.github.io`.
    /// Required only when `[x.write] enabled = true` or `[nostr] enabled = true`;
    /// otherwise optional. Do not include a trailing slash; prompt URLs append
    /// `/<category>/<slug>`.
    #[serde(default)]
    pub public_base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,

    #[serde(default)]
    pub accounts: Vec<String>,

    #[serde(default)]
    pub include_replies: bool,

    #[serde(default)]
    pub include_reposts: bool,

    #[serde(default)]
    pub ignore_patterns: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct XConfig {
    #[serde(default)]
    pub read: XReadConfig,

    #[serde(default)]
    pub write: XWriteConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XReadConfig {
    #[serde(default = "default_x_bearer_token_env")]
    pub bearer_token_env: String,

    #[serde(default = "default_x_read_max_pages")]
    pub max_pages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XWriteConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_x_mode")]
    pub mode: String,

    #[serde(default = "default_x_user_token_env")]
    pub oauth2_user_token_env: String,

    #[serde(default = "default_x_max_chars")]
    pub max_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_nostr_secret_key_env")]
    pub secret_key_env: String,

    #[serde(default)]
    pub relays: Vec<String>,
}

fn default_state_db_path() -> PathBuf {
    PathBuf::from("./state.sqlite")
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_true() -> bool {
    true
}

fn default_wiki_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wiki")
        .join("topics")
        .join("libertarian")
}

fn default_lens_path() -> PathBuf {
    default_wiki_path().join("lens-austrian-libertarian.md")
}

fn default_lens_id() -> String {
    "austrian-libertarian".to_string()
}

fn default_harness_command() -> String {
    "claude".to_string()
}

fn default_harness_args() -> Vec<String> {
    vec!["--print".to_string()]
}

fn default_prompt_template() -> PathBuf {
    PathBuf::from("./prompts/process-post.md")
}

fn default_harness_timeout() -> u64 {
    600
}

fn default_poll_interval() -> u64 {
    300
}

fn default_x_bearer_token_env() -> String {
    "X_BEARER_TOKEN".to_string()
}

fn default_x_read_max_pages() -> usize {
    5
}

fn default_x_mode() -> String {
    "reply".to_string()
}

fn default_x_user_token_env() -> String {
    "X_USER_TOKEN".to_string()
}

fn default_x_max_chars() -> usize {
    280
}

fn default_nostr_secret_key_env() -> String {
    "NOSTR_NSEC".to_string()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            state_db_path: default_state_db_path(),
            log_level: default_log_level(),
            dry_run: default_true(),
        }
    }
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            path: default_wiki_path(),
        }
    }
}

impl Default for LensConfig {
    fn default() -> Self {
        Self {
            path: default_lens_path(),
            id: default_lens_id(),
        }
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            command: default_harness_command(),
            args: default_harness_args(),
            prompt_template: default_prompt_template(),
            timeout_secs: default_harness_timeout(),
        }
    }
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            accounts: vec![],
            include_replies: false,
            include_reposts: false,
            ignore_patterns: vec![],
        }
    }
}

impl Default for XReadConfig {
    fn default() -> Self {
        Self {
            bearer_token_env: default_x_bearer_token_env(),
            max_pages: default_x_read_max_pages(),
        }
    }
}

impl Default for XWriteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_x_mode(),
            oauth2_user_token_env: default_x_user_token_env(),
            max_chars: default_x_max_chars(),
        }
    }
}

impl Default for NostrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret_key_env: default_nostr_secret_key_env(),
            relays: vec![],
        }
    }
}

impl AppConfig {
    /// Load configuration from file and environment.
    pub fn load(config_path: Option<&Path>) -> Result<Self> {
        let mut builder = config::Config::builder();

        let default_path = PathBuf::from("./config.toml");
        let path = config_path.unwrap_or(&default_path);

        if path.exists() {
            builder = builder.add_source(config::File::from(path));
        } else if config_path.is_some() {
            anyhow::bail!("Config file not found: {}", path.display());
        }

        builder = builder.add_source(
            config::Environment::with_prefix("NEWS_LENS")
                .separator("__")
                .try_parsing(true),
        );

        let config = builder.build().context("Failed to build configuration")?;

        let config: Self = config
            .try_deserialize()
            .context("Failed to deserialize configuration")?;

        config.validate()?;
        Ok(config)
    }

    /// Load only enough configuration to choose a logging fallback.
    pub fn log_level_from_config(config_path: Option<&Path>) -> Option<String> {
        Self::load(config_path)
            .ok()
            .map(|config| config.general.log_level)
    }

    /// Generate example configuration as TOML with an explicit prompt template path.
    pub fn example_toml_with_prompt_template(prompt_template_path: &Path) -> String {
        let wiki_path = escape_toml_string(&default_wiki_path().display().to_string());
        let lens_path = escape_toml_string(&default_lens_path().display().to_string());
        let prompt_template = escape_toml_string(&prompt_template_path.display().to_string());

        format!(
            r#"# news-lens configuration

[general]
state_db_path = "./state.sqlite"
log_level = "info"
dry_run = true

[wiki]
path = "{wiki_path}"

[lens]
path = "{lens_path}"
id = "austrian-libertarian"

[harness]
command = "claude"
args = ["--print"]
prompt_template = "{prompt_template}"
timeout_secs = 600

[publish]
# Base URL for sources-reply links in the X thread. Required when
# [x.write] or [nostr] is enabled.
public_base_url = "https://douglaz.github.io"

[watch]
poll_interval_secs = 300
accounts = []
include_replies = false
include_reposts = false
ignore_patterns = []

[x.read]
bearer_token_env = "X_BEARER_TOKEN"
max_pages = 5

[x.write]
enabled = false
mode = "reply"
oauth2_user_token_env = "X_USER_TOKEN"
max_chars = 280

[nostr]
enabled = false
secret_key_env = "NOSTR_NSEC"
relays = ["wss://relay.damus.io"]
"#
        )
    }

    fn validate(&self) -> Result<()> {
        if self.watch.poll_interval_secs == 0 {
            anyhow::bail!("watch.poll_interval_secs must be greater than 0");
        }
        if self.lens.id.trim().is_empty() {
            anyhow::bail!("lens.id must not be empty");
        }
        if self.harness.command.trim().is_empty() {
            anyhow::bail!("harness.command must not be empty");
        }
        if self.harness.timeout_secs == 0 {
            anyhow::bail!("harness.timeout_secs must be greater than 0");
        }
        if !(1..=10).contains(&self.x.read.max_pages) {
            anyhow::bail!("x.read.max_pages must be between 1 and 10");
        }
        let publish_required = self.x.write.enabled || self.nostr.enabled;
        let base = self.publish.public_base_url.trim();
        if !base.is_empty() {
            if !(base.starts_with("http://") || base.starts_with("https://")) {
                anyhow::bail!(
                    "publish.public_base_url must be an http(s) URL, got: {}",
                    base
                );
            }
            if base.ends_with('/') {
                anyhow::bail!(
                    "publish.public_base_url must not end with a slash, got: {}",
                    base
                );
            }
        } else if publish_required {
            anyhow::bail!(
                "publish.public_base_url is required when [x.write] or [nostr] is enabled"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_rejects_zero_poll_interval() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[watch]\npoll_interval_secs = 0\n").expect("write config");

        let error = AppConfig::load(Some(&path)).expect_err("invalid config");

        assert!(error.to_string().contains("poll_interval_secs"));
    }

    #[test]
    fn load_rejects_empty_lens_id() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[lens]\nid = \"  \"\n").expect("write config");

        let error = AppConfig::load(Some(&path)).expect_err("invalid config");

        assert!(error.to_string().contains("lens.id"));
    }

    #[test]
    fn load_rejects_empty_harness_command() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[harness]\ncommand = \"  \"\n").expect("write config");

        let error = AppConfig::load(Some(&path)).expect_err("invalid config");

        assert!(error.to_string().contains("harness.command"));
    }

    #[test]
    fn load_rejects_x_read_max_pages_above_hard_cap() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[x.read]\nmax_pages = 11\n").expect("write config");

        let error = AppConfig::load(Some(&path)).expect_err("invalid config");

        assert!(error.to_string().contains("x.read.max_pages"));
    }

    #[test]
    fn load_rejects_public_base_url_with_trailing_slash() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[publish]\npublic_base_url = \"https://example.test/\"\n",
        )
        .expect("write config");

        let error = AppConfig::load(Some(&path)).expect_err("invalid config");

        assert!(error.to_string().contains("must not end with a slash"));
    }

    #[test]
    fn example_toml_can_use_explicit_prompt_path() {
        let prompt_path = PathBuf::from("/tmp/news-lens/process-post.md");

        let toml = AppConfig::example_toml_with_prompt_template(&prompt_path);

        assert!(toml.contains("prompt_template = \"/tmp/news-lens/process-post.md\""));
    }
}
