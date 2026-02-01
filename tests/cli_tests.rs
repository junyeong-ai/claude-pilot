use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn test_cli_help() {
    let mut cmd = cargo_bin_cmd!("claude-pilot");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Mission orchestrator for Claude Code",
        ))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("mission"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn test_cli_version() {
    let mut cmd = cargo_bin_cmd!("claude-pilot");
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-pilot"));
}

#[test]
fn test_cli_mission_help() {
    let mut cmd = cargo_bin_cmd!("claude-pilot");
    cmd.args(["mission", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start a new mission"))
        .stdout(predicate::str::contains("--isolated"))
        .stdout(predicate::str::contains("--direct"))
        .stdout(predicate::str::contains("--priority"));
}

#[test]
fn test_cli_config_help() {
    let mut cmd = cargo_bin_cmd!("claude-pilot");
    cmd.args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("edit"))
        .stdout(predicate::str::contains("reset"));
}

#[test]
fn test_cli_extract_help() {
    let mut cmd = cargo_bin_cmd!("claude-pilot");
    cmd.args(["extract", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Extract learnings"))
        .stdout(predicate::str::contains("--all"))
        .stdout(predicate::str::contains("--apply"));
}
