use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
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
    cmd.current_dir(workspace_root())
        .args(["--config", "fixtures/config/stub-harness.toml", "doctor"])
        .assert()
        .success();
}
