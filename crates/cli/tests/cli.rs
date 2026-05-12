use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn fixture_config_with_state(state_path: &Path) -> String {
    fs::read_to_string(workspace_root().join("fixtures/config/stub-harness.toml"))
        .expect("fixture config")
        .replace(
            r#"state_db_path = "./target/news-lens-fixture-state.sqlite""#,
            &format!(r#"state_db_path = "{}""#, state_path.display()),
        )
}

#[test]
fn config_init_writes_example_file() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.args(["config", "init", "--path"])
        .arg(&config_path)
        .assert()
        .success();

    let content = fs::read_to_string(&config_path).expect("read config");
    assert!(content.contains("[wiki]"));
    assert!(content.contains("[harness]"));
    assert!(content.contains("dry_run = true"));
}

#[test]
fn process_post_with_stub_harness_outputs_valid_json() {
    let mut cmd = cargo_bin_cmd!("news-lens");
    let output = cmd
        .current_dir(workspace_root())
        .args([
            "process",
            "--post",
            "--text",
            "Test news item",
            "--dry-run",
            "--config",
            "fixtures/config/stub-harness.toml",
        ])
        .output()
        .expect("run process");

    assert!(output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert_eq!(value["stance"], "critique");
    assert_eq!(value["raw_path"], "raw/news/test-news-item.md");
    assert_eq!(value["thesis_slug"], "test-thesis");
}

#[test]
fn doctor_accepts_stub_fixture() {
    let mut cmd = cargo_bin_cmd!("news-lens");
    let output = cmd
        .current_dir(workspace_root())
        .args(["--config", "fixtures/config/stub-harness.toml", "doctor"])
        .output()
        .expect("run doctor");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("Config [OK]:"));
    assert!(stdout.contains("X Read [WARN]:"));
}

#[test]
fn process_jsonl_dry_run_does_not_write_state_db() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    fs::write(&config_path, fixture_config_with_state(&state_path)).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.current_dir(workspace_root())
        .args([
            "process",
            "--jsonl",
            "fixtures/posts/source_posts.jsonl",
            "--dry-run",
        ])
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();

    assert!(!state_path.exists());
}

#[test]
fn process_jsonl_configured_dry_run_does_not_write_state_db() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    fs::write(&config_path, fixture_config_with_state(&state_path)).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.current_dir(workspace_root())
        .args(["process", "--jsonl", "fixtures/posts/source_posts.jsonl"])
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();

    assert!(!state_path.exists());
}

#[test]
fn process_jsonl_with_state_writes_never_requires_direct_publisher_credentials() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    let config = fixture_config_with_state(&state_path)
        .replace("dry_run = true", "dry_run = false")
        .replace("[x.write]\nenabled = false", "[x.write]\nenabled = true");
    fs::write(&config_path, config).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.current_dir(workspace_root())
        .env_remove("X_USER_TOKEN")
        .args(["process", "--jsonl", "fixtures/posts/source_posts.jsonl"])
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();

    assert!(state_path.exists());
}

#[test]
fn process_jsonl_require_approval_writes_outbox_and_state() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    let outbox_path = dir.path().join("outbox.jsonl");
    let config = fixture_config_with_state(&state_path)
        .replace("[x.write]\nenabled = false", "[x.write]\nenabled = true");
    fs::write(&config_path, config).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    let output = cmd
        .current_dir(workspace_root())
        .args([
            "process",
            "--jsonl",
            "fixtures/posts/source_posts.jsonl",
            "--require-approval",
            "--outbox",
        ])
        .arg(&outbox_path)
        .arg("--config")
        .arg(&config_path)
        .output()
        .expect("run process jsonl");

    assert!(output.status.success());
    assert!(state_path.exists());

    let outbox = fs::read_to_string(&outbox_path).expect("read outbox");
    let value: Value = serde_json::from_str(outbox.trim()).expect("valid outbox json");
    assert_eq!(value["platform"], "x");
    assert_eq!(value["source_post_id"], "fixture-1");
    assert_eq!(
        value["text"],
        "A concise fixture line grounded in the wiki."
    );
}

#[test]
fn process_jsonl_require_approval_new_post_outbox_includes_source_link() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    let outbox_path = dir.path().join("outbox.jsonl");
    let config = fixture_config_with_state(&state_path)
        .replace("[x.write]\nenabled = false", "[x.write]\nenabled = true")
        .replace("mode = \"reply\"", "mode = \"new_post\"");
    fs::write(&config_path, config).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.current_dir(workspace_root())
        .args([
            "process",
            "--jsonl",
            "fixtures/posts/source_posts.jsonl",
            "--require-approval",
            "--outbox",
        ])
        .arg(&outbox_path)
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();

    let outbox = fs::read_to_string(&outbox_path).expect("read outbox");
    let value: Value = serde_json::from_str(outbox.trim()).expect("valid outbox json");
    assert_eq!(
        value["text"],
        "A concise fixture line grounded in the wiki.\n\nhttps://example.com/fixture-1"
    );
}

#[test]
fn run_outbox_requires_approval_mode() {
    let dir = TempDir::new().expect("temp dir");
    let outbox_path = dir.path().join("outbox.jsonl");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.args(["run", "--once", "--outbox"])
        .arg(&outbox_path)
        .assert()
        .failure();
}

#[test]
fn run_once_with_no_accounts_does_not_require_x_token() {
    let dir = TempDir::new().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    let state_path = dir.path().join("state.sqlite");
    fs::write(&config_path, fixture_config_with_state(&state_path)).expect("write config");

    let mut cmd = cargo_bin_cmd!("news-lens");
    cmd.current_dir(workspace_root())
        .env_remove("X_BEARER_TOKEN")
        .args(["run", "--once", "--dry-run"])
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();
}
