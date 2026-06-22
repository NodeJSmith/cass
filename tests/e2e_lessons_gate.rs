//! Real-binary E2E gate for `cass lessons` (bead
//! `coding_agent_session_search-guided-ops-repro-trust-5u82n.4`, "Extract
//! durable lessons and decisions from closed sessions").
//!
//! The pure cores already have unit coverage: `src/lessons.rs` (dedup +
//! supersession) and `src/lessons_extraction.rs` (classification + redaction).
//! This gate proves the wiring survives to the real `cass` binary against a
//! checked-in evidence corpus (`tests/fixtures/lessons/corpus.evidence.json`)
//! that deliberately bundles every required case:
//!
//!   * a **repeated fix** mined from both a commit and its closing bead
//!     (must dedupe to one lesson with merged provenance),
//!   * a **failed workaround** superseded by a **landed decision** on the same
//!     topic (one active, one superseded),
//!   * **outdated advice** (marked outdated, never active),
//!   * a **security warning** (preserved as its own kind),
//!   * a **high-confidence landed decision** (active), and
//!   * a body carrying a home path, an e-mail, and a long digest that must be
//!     redacted out (no raw leakage).
//!
//! Each subcommand is driven in robot mode; stdout must be pure JSON.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;

/// Generous per-surface wall-clock bound; only fires on a true hang.
const SURFACE_BOUND: Duration = Duration::from_secs(60);

/// Raw markers planted in the corpus that must never survive redaction.
const RAW_USERNAME: &str = "realuser";
const RAW_EMAIL_DOMAIN: &str = "corp.example";
const RAW_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lessons")
}

/// `cass lessons <extra...> --fixture-dir <corpus> --fixture-id corpus --json`.
fn lessons_args(extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = vec!["lessons".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    args.push("--fixture-dir".to_string());
    args.push(fixture_dir().to_string_lossy().into_owned());
    args.push("--fixture-id".to_string());
    args.push("corpus".to_string());
    args.push("--json".to_string());
    args
}

/// Run a lessons surface and return (parsed-json, raw-stdout, elapsed).
fn run_lessons(extra: &[&str]) -> Result<(Value, String, Duration), String> {
    let args = lessons_args(extra);
    let started = Instant::now();
    let out = Command::new(cargo_bin("cass"))
        .args(&args)
        .env("NO_COLOR", "1")
        .env("CASS_IGNORE_SOURCES_CONFIG", "1")
        .env("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", "1")
        .output()
        .map_err(|e| format!("spawn {extra:?}: {e}"))?;
    let elapsed = started.elapsed();
    let code = out
        .status
        .code()
        .ok_or_else(|| format!("{extra:?} killed by signal"))?;
    if code != 0 {
        return Err(format!(
            "{extra:?} exited {code}; stderr: {}",
            head(&String::from_utf8_lossy(&out.stderr))
        ));
    }
    let stdout =
        String::from_utf8(out.stdout).map_err(|e| format!("{extra:?} stdout not UTF-8: {e}"))?;
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("{extra:?} stdout not JSON: {e}; head: {}", head(&stdout)))?;
    Ok((value, stdout, elapsed))
}

fn head(s: &str) -> String {
    s.chars().take(400).collect()
}

fn u64_at(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(Value::as_u64)
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn lessons_array(v: &Value) -> Vec<Value> {
    v.get("lessons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Whether `lesson.source_refs` contains `needle`.
fn has_source_ref(lesson: &Value, needle: &str) -> bool {
    lesson
        .get("source_refs")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().filter_map(Value::as_str).any(|s| s == needle))
}

/// Failure message for a count mismatch (kept out of loop bodies on purpose).
fn count_mismatch(label: &str, got: Option<u64>, want: u64) -> String {
    format!("{label} = {got:?}, want {want}")
}

/// Failure message for an unstable lesson id.
fn unstable_id_msg(id: &str) -> String {
    format!("lesson_id not stable: {id:?}")
}

fn finish(failures: Vec<String>) -> Result<(), String> {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} check(s) failed:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        ))
    }
}

/// 1: the full graph has the expected shape: 9 candidates dedupe to 8 lessons
/// with 6 active / 1 superseded / 1 outdated, in deterministic fixture mode.
#[test]
fn lessons_list_all_has_expected_graph_shape() -> Result<(), String> {
    let (v, _raw, elapsed) = run_lessons(&["list", "--status", "all"])?;
    let mut failures: Vec<String> = Vec::new();

    if str_at(&v, "/mode") != Some("fixture") {
        failures.push(format!("mode != fixture: {:?}", str_at(&v, "/mode")));
    }
    if str_at(&v, "/project") != Some("cass") {
        failures.push(format!("project != cass: {:?}", str_at(&v, "/project")));
    }
    for (ptr, want) in [
        ("/summary/total", 8u64),
        ("/summary/active", 6),
        ("/summary/superseded", 1),
        ("/summary/outdated", 1),
        ("/manifest/candidates_emitted", 9),
        ("/manifest/commits_scanned", 5),
        ("/manifest/beads_scanned", 3),
        ("/manifest/proofs_scanned", 1),
    ] {
        if u64_at(&v, ptr) != Some(want) {
            failures.push(count_mismatch(ptr, u64_at(&v, ptr), want));
        }
    }
    // Every returned lesson carries a stable `lsn-` id.
    for lesson in lessons_array(&v) {
        let id = lesson
            .get("lesson_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !id.starts_with("lsn-") {
            failures.push(unstable_id_msg(id));
        }
    }
    if elapsed >= SURFACE_BOUND {
        failures.push(format!("list took {elapsed:?} (>= {SURFACE_BOUND:?})"));
    }
    finish(failures)
}

/// 2: the repeated fix dedupes to ONE lesson that merges both provenance refs
/// and keeps the freshest metadata.
#[test]
fn repeated_fix_dedupes_with_merged_provenance() -> Result<(), String> {
    let (v, _raw, _elapsed) = run_lessons(&["search", "preflight"])?;
    let mut failures: Vec<String> = Vec::new();

    let matches: Vec<Value> = lessons_array(&v)
        .into_iter()
        .filter(|l| has_source_ref(l, "commit:abc123") || has_source_ref(l, "bead:bd-rch-1"))
        .collect();
    if matches.len() != 1 {
        failures.push(format!(
            "expected exactly 1 rch lesson, got {}",
            matches.len()
        ));
    }
    if let Some(lesson) = matches.first() {
        if !has_source_ref(lesson, "commit:abc123") {
            failures.push("rch lesson missing commit:abc123 provenance".to_string());
        }
        if !has_source_ref(lesson, "bead:bd-rch-1") {
            failures.push("rch lesson missing bead:bd-rch-1 provenance".to_string());
        }
        if lesson.get("freshness_ms").and_then(Value::as_u64) != Some(200000) {
            failures.push("rch lesson did not keep the freshest metadata".to_string());
        }
        if lesson.get("status").and_then(Value::as_str) != Some("active") {
            failures.push("rch lesson should be active".to_string());
        }
        if lesson.get("kind").and_then(Value::as_str) != Some("gotcha") {
            failures.push("rch fix should classify as a gotcha".to_string());
        }
    }
    finish(failures)
}

/// 3: a failed workaround is superseded by the fresher landed decision on the
/// same topic; the active one is the reusable decision.
#[test]
fn failed_workaround_is_superseded_by_landed_decision() -> Result<(), String> {
    let (v, _raw, _elapsed) = run_lessons(&["list", "--status", "all"])?;
    let mut failures: Vec<String> = Vec::new();

    let topic_lessons: Vec<Value> = lessons_array(&v)
        .into_iter()
        .filter(|l| l.get("topic").and_then(Value::as_str) == Some("frankensqlite-group-by"))
        .collect();
    if topic_lessons.len() != 2 {
        failures.push(format!(
            "expected 2 frankensqlite-group-by lessons, got {}",
            topic_lessons.len()
        ));
    }
    let active: Vec<&Value> = topic_lessons
        .iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("active"))
        .collect();
    let superseded: Vec<&Value> = topic_lessons
        .iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("superseded"))
        .collect();
    if active.len() != 1 {
        failures.push(format!(
            "expected 1 active on the topic, got {}",
            active.len()
        ));
    }
    if superseded.len() != 1 {
        failures.push(format!(
            "expected 1 superseded on the topic, got {}",
            superseded.len()
        ));
    }
    if let Some(a) = active.first()
        && a.get("kind").and_then(Value::as_str) != Some("reusable_decision")
    {
        failures.push("active topic lesson should be the reusable decision".to_string());
    }
    if let Some(s) = superseded.first()
        && s.get("kind").and_then(Value::as_str) != Some("failed_approach")
    {
        failures.push("superseded topic lesson should be the failed approach".to_string());
    }
    finish(failures)
}

/// 4: outdated advice is marked outdated and is absent from the active view.
#[test]
fn outdated_advice_is_marked_and_excluded_from_active() -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();

    let (outdated, _raw, _e) = run_lessons(&["list", "--status", "outdated"])?;
    let outdated_lessons = lessons_array(&outdated);
    if outdated_lessons.len() != 1 {
        failures.push(format!(
            "expected 1 outdated lesson, got {}",
            outdated_lessons.len()
        ));
    }
    if let Some(l) = outdated_lessons.first() {
        if l.get("topic").and_then(Value::as_str) != Some("rch-local-patch") {
            failures.push("outdated lesson topic mismatch".to_string());
        }
        if l.get("status").and_then(Value::as_str) != Some("outdated") {
            failures.push("outdated lesson status mismatch".to_string());
        }
    }

    // The same lesson must not appear in the active view.
    let (active, _raw2, _e2) = run_lessons(&["list", "--status", "active"])?;
    let leaked = lessons_array(&active)
        .into_iter()
        .any(|l| l.get("topic").and_then(Value::as_str) == Some("rch-local-patch"));
    if leaked {
        failures.push("outdated advice leaked into the active view".to_string());
    }
    finish(failures)
}

/// 5: the security warning survives classification as its own kind.
#[test]
fn security_warning_is_preserved_as_its_own_kind() -> Result<(), String> {
    let (v, _raw, _e) = run_lessons(&["list", "--status", "all", "--kind", "security_warning"])?;
    let mut failures: Vec<String> = Vec::new();

    let secs = lessons_array(&v);
    if secs.len() != 1 {
        failures.push(format!(
            "expected 1 security_warning lesson, got {}",
            secs.len()
        ));
    }
    if let Some(l) = secs.first() {
        if l.get("topic").and_then(Value::as_str) != Some("update") {
            failures.push("security lesson topic mismatch".to_string());
        }
        if l.get("kind").and_then(Value::as_str) != Some("security_warning") {
            failures.push("security lesson kind mismatch".to_string());
        }
    }
    finish(failures)
}

/// 6: redaction strips the planted markers; the report counts them and no raw
/// username / e-mail / digest survives anywhere in the output.
#[test]
fn redaction_removes_planted_markers_and_counts_them() -> Result<(), String> {
    let (v, raw, _e) = run_lessons(&["list", "--status", "all"])?;
    let mut failures: Vec<String> = Vec::new();

    for marker in [RAW_USERNAME, RAW_EMAIL_DOMAIN, RAW_DIGEST] {
        if raw.contains(marker) {
            failures.push(format!("raw marker leaked into output: {marker:?}"));
        }
    }
    if u64_at(&v, "/redaction/home_paths").unwrap_or(0) < 1 {
        failures.push("redaction.home_paths should be >= 1".to_string());
    }
    if u64_at(&v, "/redaction/emails").unwrap_or(0) < 1 {
        failures.push("redaction.emails should be >= 1".to_string());
    }
    if u64_at(&v, "/redaction/digests").unwrap_or(0) < 1 {
        failures.push("redaction.digests should be >= 1".to_string());
    }
    // The redacted export lesson is still present and useful.
    let has_export = lessons_array(&v)
        .into_iter()
        .any(|l| has_source_ref(&l, "commit:leak01"));
    if !has_export {
        failures.push("redacted export lesson is missing".to_string());
    }
    finish(failures)
}

/// 7: search finds the right lessons and reports a match count.
#[test]
fn search_finds_expected_lessons() -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();

    let (inj, _r, _e) = run_lessons(&["search", "injection"])?;
    if u64_at(&inj, "/matched").unwrap_or(0) < 1 {
        failures.push("search 'injection' matched nothing".to_string());
    }
    let found_sec = lessons_array(&inj)
        .into_iter()
        .any(|l| l.get("kind").and_then(Value::as_str) == Some("security_warning"));
    if !found_sec {
        failures.push("search 'injection' did not surface the security warning".to_string());
    }

    // A query that matches nothing returns an empty, well-formed payload.
    let (none, _r2, _e2) = run_lessons(&["search", "zzz-no-such-token-zzz"])?;
    if u64_at(&none, "/matched") != Some(0) {
        failures.push("no-match search should report matched=0".to_string());
    }
    if !lessons_array(&none).is_empty() {
        failures.push("no-match search should return an empty lessons array".to_string());
    }
    finish(failures)
}

/// 8: view round-trips a lesson id discovered from list.
#[test]
fn view_round_trips_a_lesson_id() -> Result<(), String> {
    let (list, _r, _e) = run_lessons(&["list", "--status", "all"])?;
    let mut failures: Vec<String> = Vec::new();

    let id = lessons_array(&list).into_iter().find_map(|l| {
        l.get("lesson_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let Some(id) = id else {
        return Err("no lesson id available to view".to_string());
    };

    let (view, _r2, _e2) = run_lessons(&["view", &id])?;
    if view.get("found").and_then(Value::as_bool) != Some(true) {
        failures.push(format!("view of {id} reported not found"));
    }
    if str_at(&view, "/lesson/lesson_id") != Some(id.as_str()) {
        failures.push("view returned a different lesson id".to_string());
    }

    // A bogus id is a clean not-found, not a crash or stdout pollution.
    let (missing, _r3, _e3) = run_lessons(&["view", "lsn-deadbeefdeadbeef"])?;
    if missing.get("found").and_then(Value::as_bool) != Some(false) {
        failures.push("view of a bogus id should report found=false".to_string());
    }
    finish(failures)
}
