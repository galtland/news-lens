//! Doctor command.

use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

use crate::args::DoctorArgs;
use crate::commands::common::{command_exists, load_configured_lens};
use crate::config::AppConfig;

#[derive(Debug, Serialize)]
struct DoctorReport {
    config: CheckResult,
    wiki: CheckResult,
    lens: CheckResult,
    harness: CheckResult,
    x_read: CheckResult,
    x_write: CheckResult,
    nostr: CheckResult,
    overall: String,
}

#[derive(Debug, Serialize)]
struct CheckResult {
    status: String,
    message: String,
}

impl CheckResult {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            status: "ok".to_string(),
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            status: "warn".to_string(),
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            message: message.into(),
        }
    }

    fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    fn is_error(&self) -> bool {
        self.status == "error"
    }
}

pub async fn execute(args: DoctorArgs, config_path: Option<PathBuf>) -> Result<()> {
    let mut report = DoctorReport {
        config: CheckResult::error("Not checked"),
        wiki: CheckResult::error("Not checked"),
        lens: CheckResult::error("Not checked"),
        harness: CheckResult::error("Not checked"),
        x_read: CheckResult::error("Not checked"),
        x_write: CheckResult::error("Not checked"),
        nostr: CheckResult::error("Not checked"),
        overall: "error".to_string(),
    };

    let config = match AppConfig::load(config_path.as_deref()) {
        Ok(config) => {
            report.config = CheckResult::ok("Configuration loaded successfully");
            Some(config)
        }
        Err(error) => {
            report.config = CheckResult::error(format!("Failed to load config: {}", error));
            None
        }
    };

    if let Some(ref config) = config {
        report.wiki = check_wiki(config);
        report.lens = check_lens(config);
        report.harness = check_harness(config);
        report.x_read = check_x_read(config);
        report.x_write = check_x_write(config);
        report.nostr = check_nostr(config);
    }

    let checks = [
        &report.config,
        &report.wiki,
        &report.lens,
        &report.harness,
        &report.x_read,
    ];
    let has_error = checks.iter().any(|check| check.is_error());
    let all_ok = checks.iter().all(|check| check.is_ok());

    report.overall = if has_error {
        "error".to_string()
    } else if all_ok {
        "ok".to_string()
    } else {
        "warn".to_string()
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    if report.overall == "error" {
        std::process::exit(1);
    }

    Ok(())
}

fn check_wiki(config: &AppConfig) -> CheckResult {
    if config.wiki.path.is_dir() {
        CheckResult::ok(format!(
            "Wiki path readable: {}",
            config.wiki.path.display()
        ))
    } else {
        CheckResult::error(format!(
            "Wiki path is not a directory: {}",
            config.wiki.path.display()
        ))
    }
}

fn check_lens(config: &AppConfig) -> CheckResult {
    match load_configured_lens(config) {
        Ok(lens) => CheckResult::ok(format!("Lens loaded: {}", lens.id)),
        Err(error) => CheckResult::error(error.to_string()),
    }
}

fn check_harness(config: &AppConfig) -> CheckResult {
    if config.harness.command.trim().is_empty() {
        return CheckResult::error("Harness command is empty");
    }
    if !config.harness.prompt_template.is_file() {
        return CheckResult::error(format!(
            "Prompt template not found: {}",
            config.harness.prompt_template.display()
        ));
    }
    if command_exists(&config.harness.command) {
        CheckResult::ok(format!("Harness command found: {}", config.harness.command))
    } else {
        CheckResult::warn(format!(
            "Harness command not found on PATH: {}",
            config.harness.command
        ))
    }
}

fn check_x_read(config: &AppConfig) -> CheckResult {
    let env_var = &config.x.read.bearer_token_env;
    if config.watch.accounts.is_empty() {
        return CheckResult::warn("No accounts configured to watch");
    }
    match std::env::var(env_var) {
        Ok(value) if !value.is_empty() => {
            CheckResult::ok(format!("Bearer token: {} (set)", env_var))
        }
        _ => CheckResult::warn(format!("Bearer token: {} (not set)", env_var)),
    }
}

fn check_x_write(config: &AppConfig) -> CheckResult {
    if !config.x.write.enabled {
        return CheckResult::ok("X write disabled");
    }
    let env_var = &config.x.write.oauth2_user_token_env;
    match std::env::var(env_var) {
        Ok(value) if !value.is_empty() => CheckResult::ok(format!("User token: {} (set)", env_var)),
        _ => CheckResult::warn(format!("User token: {} (not set)", env_var)),
    }
}

fn check_nostr(config: &AppConfig) -> CheckResult {
    if !config.nostr.enabled {
        return CheckResult::ok("Nostr disabled");
    }
    if config.nostr.relays.is_empty() {
        return CheckResult::warn("No Nostr relays configured");
    }
    let env_var = &config.nostr.secret_key_env;
    match std::env::var(env_var) {
        Ok(value) if !value.is_empty() => CheckResult::ok(format!("Secret key: {} (set)", env_var)),
        _ => CheckResult::warn(format!("Secret key: {} (not set)", env_var)),
    }
}

fn print_report(report: &DoctorReport) {
    println!("news-lens Doctor Report");
    println!("=======================");
    println!();
    print_check("Config", &report.config);
    print_check("Wiki", &report.wiki);
    print_check("Lens", &report.lens);
    print_check("Harness", &report.harness);
    print_check("X Read", &report.x_read);
    print_check("X Write", &report.x_write);
    print_check("Nostr", &report.nostr);
    println!();
    println!("Overall: {}", report.overall.to_uppercase());
}

fn print_check(name: &str, result: &CheckResult) {
    println!("{}: {}", name, result.message);
}
