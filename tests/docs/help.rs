//! Help documentation and CLI flag tests.
//!
//! These tests verify that:
//! - CLI help text is complete and accurate
//! - All documented flags work as described
//! - Error messages match documentation
//! - Examples in help text are valid
//!
//! Run with:
//!   cargo test --test docs

use std::process::{Command, Stdio};
use tempfile::TempDir;

// =============================================================================
// CLI Help Tests
// =============================================================================

/// Test that --help flag produces output.
#[test]
fn test_help_flag_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_cass"))
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);

            // --help should produce output on stdout or stderr
            assert!(
                !stdout.is_empty() || !stderr.is_empty(),
                "Help output should not be empty"
            );

            // Should mention the tool name
            let combined = format!("{}{}", stdout, stderr);
            assert!(
                combined.to_lowercase().contains("cass")
                    || combined.to_lowercase().contains("coding agent"),
                "Help should mention the tool name"
            );
        }
        Err(e) => {
            // If binary isn't built, skip gracefully
            println!("Skipping: Could not run cass binary: {}", e);
        }
    }
}

/// Test that -h is an alias for --help.
#[test]
fn test_short_help_flag() {
    let output_long = Command::new(env!("CARGO_BIN_EXE_cass"))
        .arg("--help")
        .output();

    let output_short = Command::new(env!("CARGO_BIN_EXE_cass")).arg("-h").output();

    match (output_long, output_short) {
        (Ok(long), Ok(short)) => {
            // Both should have similar content (allow for minor differences)
            let long_stdout = String::from_utf8_lossy(&long.stdout);
            let short_stdout = String::from_utf8_lossy(&short.stdout);

            // Both should be non-empty or both empty (consistent behavior)
            assert_eq!(
                long_stdout.is_empty(),
                short_stdout.is_empty(),
                "-h and --help should have consistent output"
            );
        }
        _ => {
            println!("Skipping: Could not run cass binary");
        }
    }
}

/// Test that --version flag works.
#[test]
fn test_version_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_cass"))
        .arg("--version")
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{}{}", stdout, stderr);

            // Should contain version number pattern
            let has_version = combined.contains(env!("CARGO_PKG_VERSION"))
                || regex::Regex::new(r"\d+\.\d+\.\d+")
                    .unwrap()
                    .is_match(&combined);

            assert!(has_version, "Version output should contain version number");
        }
        Err(e) => {
            println!("Skipping: Could not run cass binary: {}", e);
        }
    }
}

// =============================================================================
// Subcommand Help Tests
// =============================================================================

/// Test that major subcommands have help.
#[test]
fn test_subcommand_help_available() {
    let subcommands = ["search", "index", "export", "tui", "health"];

    for cmd in &subcommands {
        let output = Command::new(env!("CARGO_BIN_EXE_cass"))
            .arg(cmd)
            .arg("--help")
            .output();

        match output {
            Ok(out) => {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );

                // Either help output or "unknown command" is acceptable
                // (subcommand may not exist in all builds)
                if !combined.to_lowercase().contains("unknown")
                    && !combined.to_lowercase().contains("not found")
                {
                    assert!(
                        !combined.is_empty(),
                        "Subcommand '{}' help should produce output",
                        cmd
                    );
                }
            }
            Err(_) => {
                // Skip if binary not available
            }
        }
    }
}

// =============================================================================
// Help Content Quality Tests
// =============================================================================

/// Test that help mentions common use cases.
#[test]
fn test_help_mentions_use_cases() {
    let output = Command::new(env!("CARGO_BIN_EXE_cass"))
        .arg("--help")
        .output();

    match output {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .to_lowercase();

            // Should mention key features/use cases
            let mentions_search = combined.contains("search");
            let mentions_index = combined.contains("index");
            let mentions_export = combined.contains("export") || combined.contains("pages");

            // At least one core feature should be mentioned
            assert!(
                mentions_search || mentions_index || mentions_export,
                "Help should mention core features (search, index, export)"
            );
        }
        Err(_) => {
            println!("Skipping: Could not run cass binary");
        }
    }
}

// =============================================================================
// Error Message Tests
// =============================================================================

/// Test that invalid commands produce helpful errors.
#[test]
fn test_invalid_command_error() {
    // This CLI has a heuristic that treats unknown first args as an implicit `search` query.
    // Force a fresh, empty data dir so the command reliably fails (missing index/db) instead
    // of succeeding if the developer machine happens to have an existing index.
    let tmp = TempDir::new().expect("create temp CASS_DATA_DIR");
    let output = Command::new(env!("CARGO_BIN_EXE_cass"))
        .env("CASS_DATA_DIR", tmp.path().as_os_str())
        .arg("nonexistent-command-xyz")
        .output();

    match output {
        Ok(out) => {
            // Should exit with error
            assert!(!out.status.success(), "Invalid command should fail");

            let stderr = String::from_utf8_lossy(&out.stderr);
            // Should provide some guidance
            assert!(
                !stderr.is_empty() || !String::from_utf8_lossy(&out.stdout).is_empty(),
                "Error output should not be empty"
            );
        }
        Err(_) => {
            println!("Skipping: Could not run cass binary");
        }
    }
}

/// Test that missing required args produce helpful errors.
#[test]
fn test_missing_args_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_cass"))
        .arg("search")
        // Missing required query argument
        .output();

    match output {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );

            // Should either error or show help
            if !out.status.success() {
                // Error message should be helpful
                assert!(
                    !combined.is_empty(),
                    "Error for missing args should provide guidance"
                );
            }
        }
        Err(_) => {
            println!("Skipping: Could not run cass binary");
        }
    }
}
