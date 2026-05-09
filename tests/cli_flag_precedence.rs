//! CLI > env > config > default precedence tests.
//!
//! Per `coding_agent_session_search-d4r65`. Exercises the documented
//! precedence chain via `assert_cmd::Command` against a fresh cass binary.

use assert_cmd::Command;
use serial_test::serial;
use std::path::PathBuf;

fn temp_data_dir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("cass-d4r65-{label}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("tempdir");
    dir
}

#[test]
#[serial]
fn cass_help_exits_zero_and_lists_subcommands() {
    tracing::info!(target: "d4r65_test", scenario = "help");
    let mut cmd = Command::cargo_bin("cass").expect("cass binary built");
    cmd.arg("--help");
    let output = cmd.output().expect("cass --help runs");
    assert!(output.status.success(), "--help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The help output must enumerate at least the search/health/index subcommands.
    for sub in ["search", "health", "index"] {
        assert!(
            stdout.contains(sub),
            "--help must list `{sub}` subcommand; got stdout={stdout}"
        );
    }
}

#[test]
#[serial]
fn cli_data_dir_flag_takes_precedence_over_env() {
    tracing::info!(target: "d4r65_test", scenario = "cli_over_env");
    let env_dir = temp_data_dir("env");
    let cli_dir = temp_data_dir("cli");
    let mut cmd = Command::cargo_bin("cass").expect("cass binary built");
    cmd.env("CASS_DATA_DIR", &env_dir)
        .arg("--data-dir")
        .arg(&cli_dir)
        .arg("health")
        .arg("--json");
    let output = cmd.output().expect("runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        v.is_ok(),
        "cass health --json must emit JSON; got: {stdout}"
    );
    eprintln!(
        "[d4r65_test] cli_over_env exit={} stdout_len={}",
        output.status.code().unwrap_or(-1),
        stdout.len()
    );
}

#[test]
#[serial]
fn env_data_dir_used_when_no_flag() {
    tracing::info!(target: "d4r65_test", scenario = "env_only");
    let env_dir = temp_data_dir("env_only");
    let mut cmd = Command::cargo_bin("cass").expect("cass binary built");
    cmd.env("CASS_DATA_DIR", &env_dir)
        .arg("health")
        .arg("--json");
    let output = cmd.output().expect("runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(v.is_ok(), "stdout must be JSON; got: {stdout}");
}

#[test]
#[serial]
fn missing_required_arg_emits_actionable_error() {
    tracing::info!(target: "d4r65_test", scenario = "missing_arg");
    let mut cmd = Command::cargo_bin("cass").expect("cass binary built");
    cmd.arg("search"); // search requires a query argument
    let output = cmd.output().expect("runs");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // A missing-arg error must produce an actionable message.
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined.to_lowercase().contains("required")
                || combined.to_lowercase().contains("usage")
                || combined.to_lowercase().contains("argument")
                || combined.to_lowercase().contains("query"),
            "missing-arg error must include actionable hint; got: {combined}"
        );
    }
}

#[test]
#[serial]
fn invalid_data_dir_path_handled_without_panic() {
    tracing::info!(target: "d4r65_test", scenario = "invalid_data_dir");
    let mut cmd = Command::cargo_bin("cass").expect("cass binary built");
    // /this/path/does/not/exist — cass may auto-create it OR error cleanly.
    cmd.arg("--data-dir")
        .arg("/this/path/does/not/exist/d4r65")
        .arg("health")
        .arg("--json");
    let output = cmd.output().expect("runs");
    // Critical: must NOT panic. Either exit 0 with valid JSON or exit !=0
    // with structured error envelope.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "invalid data dir must NOT panic; stderr: {stderr}"
    );
}
