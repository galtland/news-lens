//! CLI argument definitions.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// news-lens: wiki-grounded news commentary.
#[derive(Parser, Debug)]
#[command(name = "news-lens")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to configuration file.
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error).
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Fetch new posts, process them with the harness, and optionally publish.
    Run(RunArgs),

    /// Process one synthetic post or a JSONL fixture without direct publishing.
    Process(ProcessArgs),

    /// Inspect the configured wiki.
    Wiki(WikiArgs),

    /// Inspect configured lens files.
    Lens(LensArgs),

    /// Configuration management.
    Config(ConfigArgs),

    /// Validate configuration and show status.
    Doctor(DoctorArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Run in dry-run mode (no publishing).
    #[arg(long)]
    pub dry_run: bool,

    /// Process one poll cycle and exit.
    #[arg(long)]
    pub once: bool,

    /// Write rendered posts to outbox for review instead of publishing.
    #[arg(long)]
    pub require_approval: bool,

    /// Path to outbox file (used with --require-approval).
    #[arg(long, requires = "require_approval")]
    pub outbox: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ProcessArgs {
    /// Process one ad-hoc post. The optional value is used as text unless --text is set.
    #[arg(long, num_args = 0..=1, conflicts_with = "jsonl")]
    pub post: Option<Option<String>>,

    /// Text for --post.
    #[arg(long, requires = "post")]
    pub text: Option<String>,

    /// Process posts from a SourcePost JSONL fixture.
    #[arg(long, conflicts_with = "post")]
    pub jsonl: Option<PathBuf>,

    /// For JSONL, skip state DB writes and suppress outbox publishing. --post never writes state or publishes.
    #[arg(long, conflicts_with = "require_approval")]
    pub dry_run: bool,

    /// For JSONL processing, write rendered posts to outbox for review; never publish directly.
    #[arg(long, requires = "jsonl")]
    pub require_approval: bool,

    /// Path to outbox file (used with --require-approval).
    #[arg(long, requires = "require_approval")]
    pub outbox: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct WikiArgs {
    #[command(subcommand)]
    pub command: WikiCommands,
}

#[derive(Subcommand, Debug)]
pub enum WikiCommands {
    /// Show wiki path and simple content counts.
    Status,
}

#[derive(Args, Debug)]
pub struct LensArgs {
    #[command(subcommand)]
    pub command: LensCommands,
}

#[derive(Subcommand, Debug)]
pub enum LensCommands {
    /// List lens markdown files near the configured lens.
    List,

    /// Show one lens by id.
    Show { id: String },
}

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Generate example configuration file.
    Init {
        /// Path to write config file.
        #[arg(long, default_value = "./config.toml")]
        path: PathBuf,

        /// Overwrite existing file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}
