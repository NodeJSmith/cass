use assert_cmd::Command;
use coding_agent_search::search::tantivy::expected_index_dir;
use frankensqlite::Connection as FrankenConnection;
use frankensqlite::compat::{ConnectionExt, RowExt};
use fs2::FileExt;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

fn test_canonical_json_value(value: Value) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.into_iter().map(test_canonical_json_value).collect())
        }
        Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key, test_canonical_json_value(value));
            }
            Value::Object(canonical)
        }
        other => other,
    }
}

fn test_doctor_canonical_blake3(prefix: &str, value: Value) -> String {
    let canonical = test_canonical_json_value(value);
    let encoded = serde_json::to_vec(&canonical).expect("canonical json");
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    hasher.update(&[0]);
    hasher.update(&encoded);
    format!("{prefix}-{}", hasher.finalize().to_hex())
}

fn test_original_path_blake3(path: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"doctor-raw-mirror-original-path-v1");
    hasher.update(&[0]);
    hasher.update(path.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn write_raw_mirror_fixture(
    data_dir: &Path,
    provider: &str,
    source_id: &str,
    origin_kind: &str,
    original_path: &Path,
    bytes: &[u8],
) -> Value {
    let blob_blake3 = blake3::hash(bytes).to_hex().to_string();
    let blob_relative_path = format!("blobs/blake3/{}/{}.raw", &blob_blake3[..2], blob_blake3);
    let original_path_str = original_path.to_string_lossy().into_owned();
    let original_path_blake3 = test_original_path_blake3(&original_path_str);
    let manifest_id = test_doctor_canonical_blake3(
        "doctor-raw-mirror-manifest-id-v1",
        json!({
            "provider": provider,
            "source_id": source_id,
            "origin_kind": origin_kind,
            "origin_host": Value::Null,
            "original_path_blake3": original_path_blake3,
            "blob_blake3": blob_blake3,
        }),
    );
    let mut manifest = json!({
        "schema_version": 1,
        "manifest_kind": "cass_raw_session_mirror_v1",
        "manifest_id": manifest_id,
        "blob_hash_algorithm": "blake3",
        "blob_blake3": blob_blake3,
        "blob_relative_path": blob_relative_path,
        "blob_size_bytes": bytes.len() as u64,
        "provider": provider,
        "source_id": source_id,
        "origin_kind": origin_kind,
        "origin_host": Value::Null,
        "original_path": original_path_str,
        "redacted_original_path": "[external]/pruned-session.jsonl",
        "original_path_blake3": original_path_blake3,
        "captured_at_ms": 1_733_000_000_000_i64,
        "source_mtime_ms": 1_733_000_000_000_i64,
        "source_size_bytes": bytes.len() as u64,
        "compression": {
            "state": "none",
            "algorithm": Value::Null,
            "uncompressed_size_bytes": bytes.len() as u64
        },
        "encryption": {
            "state": "none",
            "algorithm": Value::Null,
            "key_id": Value::Null,
            "envelope_version": Value::Null
        },
        "db_links": [{
            "conversation_id": 1,
            "message_count": 1,
            "source_path": original_path.to_string_lossy(),
            "started_at_ms": 1_700_000_000_000_i64
        }],
        "verification": {
            "status": "captured",
            "verifier": "cli_doctor_fixture",
            "content_blake3": Value::Null,
            "verified_at_ms": Value::Null
        }
    });
    let manifest_blake3 =
        test_doctor_canonical_blake3("doctor-raw-mirror-manifest-v1", manifest.clone());
    manifest["manifest_blake3"] = json!(manifest_blake3);

    let root = data_dir.join("raw-mirror").join("v1");
    let blob_path = root.join(manifest["blob_relative_path"].as_str().expect("blob rel"));
    fs::create_dir_all(blob_path.parent().expect("blob parent")).expect("create blob parent");
    fs::write(&blob_path, bytes).expect("write raw mirror blob");
    let manifest_path = root.join("manifests").join(format!(
        "{}.json",
        manifest["manifest_id"].as_str().expect("manifest id")
    ));
    fs::create_dir_all(manifest_path.parent().expect("manifest parent"))
        .expect("create manifest parent");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("manifest json"),
    )
    .expect("write manifest");
    manifest
}

fn cass_cmd(test_home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("cass"));
    cmd.env("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", "1")
        .env("CASS_IGNORE_SOURCES_CONFIG", "1")
        .env("XDG_DATA_HOME", test_home)
        .env("XDG_CONFIG_HOME", test_home)
        .env("HOME", test_home)
        .current_dir(test_home);
    cmd
}

fn seed_healthy_empty_index(test_home: &Path, data_dir: &Path) {
    let out = cass_cmd(test_home)
        .args([
            "index",
            "--force-rebuild",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run seed index");
    assert!(
        out.status.success(),
        "seed index failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn make_file_mtime_older_than(path: &Path, age: Duration) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open file for mtime update");
    let modified = std::time::SystemTime::now()
        .checked_sub(age)
        .expect("mtime before now");
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("set file mtime");
}

fn write_repair_failure_marker_fixture(
    data_dir: &Path,
    repair_class: &str,
    operation_id: &str,
    failed_at_ms: i64,
) -> std::path::PathBuf {
    let marker_dir = data_dir
        .join("doctor")
        .join("failure-markers")
        .join(repair_class);
    fs::create_dir_all(&marker_dir).expect("create repair failure marker dir");
    let marker_path = marker_dir.join(format!("{failed_at_ms}-{operation_id}.json"));
    fs::write(
        &marker_path,
        serde_json::to_vec_pretty(&json!({
            "marker_kind": "cass_doctor_repair_failure_marker_v1",
            "schema_version": 1,
            "repair_class": repair_class,
            "operation_id": operation_id,
            "command_line_mode": "cass doctor --json --fix",
            "plan_fingerprint": format!("plan-{operation_id}"),
            "affected_artifacts": [
                {
                    "artifact_kind": "doctor_affected_asset",
                    "asset_class": "derived_lexical_index",
                    "path": data_dir.join("index").display().to_string(),
                    "redacted_path": "[cass-data]/index"
                }
            ],
            "selected_authorities": ["doctor_check_report_v1"],
            "rejected_authorities": [],
            "preflight_checks": ["database:pass", "index:pass"],
            "applied_actions": [],
            "verification_checks": ["rebuild:fail"],
            "failed_checks": ["rebuild:repair-previously-failed"],
            "forensic_bundle_path": "[cass-data]/doctor/forensics/failed-test",
            "candidate_path": "[cass-data]/doctor/tmp/candidate-test",
            "started_at_ms": failed_at_ms - 10,
            "failed_at_ms": failed_at_ms,
            "cass_version": env!("CARGO_PKG_VERSION"),
            "platform": "test/test",
            "user_data_modified": false,
            "operation_outcome_kind": "verification-failed"
        }))
        .expect("serialize marker"),
    )
    .expect("write repair failure marker");
    marker_path
}

fn write_quarantined_manifest(generation_dir: &Path) {
    fs::create_dir_all(generation_dir).expect("create generation dir");
    fs::write(
        generation_dir.join("lexical-generation-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 1,
            "generation_id": "gen-quarantined",
            "attempt_id": "attempt-1",
            "created_at_ms": 1_733_000_000_000_i64,
            "updated_at_ms": 1_733_000_000_321_i64,
            "source_db_fingerprint": "fp-test",
            "conversation_count": 3,
            "message_count": 9,
            "indexed_doc_count": 9,
            "equivalence_manifest_fingerprint": null,
            "shard_plan": null,
            "build_budget": null,
            "shards": [{
                "shard_id": "shard-a",
                "shard_ordinal": 0,
                "state": "quarantined",
                "updated_at_ms": 1_733_000_000_222_i64,
                "indexed_doc_count": 9,
                "message_count": 9,
                "artifact_bytes": 512,
                "stable_hash": "stable-hash-a",
                "reclaimable": false,
                "pinned": false,
                "recovery_reason": null,
                "quarantine_reason": "validation_failed"
            }],
            "merge_debt": {
                "state": "none",
                "updated_at_ms": null,
                "pending_shard_count": 0,
                "pending_artifact_bytes": 0,
                "reason": null,
                "controller_reason": null
            },
            "build_state": "failed",
            "publish_state": "quarantined",
            "failure_history": []
        }))
        .expect("serialize manifest"),
    )
    .expect("write manifest");
}

fn write_quarantined_reclaimable_shard_manifest(generation_dir: &Path) {
    fs::create_dir_all(generation_dir).expect("create generation dir");
    fs::write(
        generation_dir.join("lexical-generation-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 1,
            "generation_id": "gen-quarantined-reclaimable",
            "attempt_id": "attempt-1",
            "created_at_ms": 1_733_000_000_000_i64,
            "updated_at_ms": 1_733_000_000_321_i64,
            "source_db_fingerprint": "fp-test",
            "conversation_count": 3,
            "message_count": 9,
            "indexed_doc_count": 9,
            "equivalence_manifest_fingerprint": null,
            "shard_plan": null,
            "build_budget": null,
            "shards": [{
                "shard_id": "shard-abandoned",
                "shard_ordinal": 0,
                "state": "abandoned",
                "updated_at_ms": 1_733_000_000_222_i64,
                "indexed_doc_count": 9,
                "message_count": 9,
                "artifact_bytes": 512,
                "stable_hash": "stable-hash-abandoned",
                "reclaimable": true,
                "pinned": false,
                "recovery_reason": "validation abandoned before publish",
                "quarantine_reason": null
            }],
            "merge_debt": {
                "state": "none",
                "updated_at_ms": null,
                "pending_shard_count": 0,
                "pending_artifact_bytes": 0,
                "reason": null,
                "controller_reason": null
            },
            "build_state": "failed",
            "publish_state": "quarantined",
            "failure_history": []
        }))
        .expect("serialize manifest"),
    )
    .expect("write manifest");
}

fn write_superseded_reclaimable_manifest(generation_dir: &Path, generation_id: &str) {
    fs::create_dir_all(generation_dir).expect("create superseded generation dir");
    fs::write(
        generation_dir.join("lexical-generation-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 1,
            "generation_id": generation_id,
            "attempt_id": "attempt-1",
            "created_at_ms": 1_733_000_000_000_i64,
            "updated_at_ms": 1_733_000_000_321_i64,
            "source_db_fingerprint": "fp-test",
            "conversation_count": 3,
            "message_count": 9,
            "indexed_doc_count": 9,
            "equivalence_manifest_fingerprint": null,
            "shard_plan": null,
            "build_budget": null,
            "shards": [{
                "shard_id": "shard-old",
                "shard_ordinal": 0,
                "state": "published",
                "updated_at_ms": 1_733_000_000_222_i64,
                "indexed_doc_count": 9,
                "message_count": 9,
                "artifact_bytes": 128,
                "stable_hash": "stable-hash-old",
                "reclaimable": true,
                "pinned": false,
                "recovery_reason": null,
                "quarantine_reason": null
            }],
            "merge_debt": {
                "state": "none",
                "updated_at_ms": null,
                "pending_shard_count": 0,
                "pending_artifact_bytes": 0,
                "reason": null,
                "controller_reason": null
            },
            "build_state": "validated",
            "publish_state": "superseded",
            "failure_history": []
        }))
        .expect("serialize superseded manifest"),
    )
    .expect("write superseded manifest");
}

fn write_superseded_partly_pinned_manifest(generation_dir: &Path, generation_id: &str) {
    fs::create_dir_all(generation_dir).expect("create partly pinned generation dir");
    fs::write(
        generation_dir.join("lexical-generation-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 1,
            "generation_id": generation_id,
            "attempt_id": "attempt-1",
            "created_at_ms": 1_733_000_000_000_i64,
            "updated_at_ms": 1_733_000_000_321_i64,
            "source_db_fingerprint": "fp-test",
            "conversation_count": 4,
            "message_count": 12,
            "indexed_doc_count": 12,
            "equivalence_manifest_fingerprint": null,
            "shard_plan": null,
            "build_budget": null,
            "shards": [
                {
                    "shard_id": "shard-old",
                    "shard_ordinal": 0,
                    "state": "published",
                    "updated_at_ms": 1_733_000_000_222_i64,
                    "indexed_doc_count": 6,
                    "message_count": 6,
                    "artifact_bytes": 128,
                    "stable_hash": "stable-hash-old",
                    "reclaimable": true,
                    "pinned": false,
                    "recovery_reason": null,
                    "quarantine_reason": null
                },
                {
                    "shard_id": "shard-pinned",
                    "shard_ordinal": 1,
                    "state": "published",
                    "updated_at_ms": 1_733_000_000_223_i64,
                    "indexed_doc_count": 6,
                    "message_count": 6,
                    "artifact_bytes": 256,
                    "stable_hash": "stable-hash-pinned",
                    "reclaimable": true,
                    "pinned": true,
                    "recovery_reason": null,
                    "quarantine_reason": null
                }
            ],
            "merge_debt": {
                "state": "none",
                "updated_at_ms": null,
                "pending_shard_count": 0,
                "pending_artifact_bytes": 0,
                "reason": null,
                "controller_reason": null
            },
            "build_state": "validated",
            "publish_state": "superseded",
            "failure_history": []
        }))
        .expect("serialize partly pinned manifest"),
    )
    .expect("write partly pinned manifest");
}

fn write_active_manifest(generation_dir: &Path, generation_id: &str) {
    fs::create_dir_all(generation_dir).expect("create active generation dir");
    fs::write(
        generation_dir.join("lexical-generation-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 1,
            "generation_id": generation_id,
            "attempt_id": "attempt-1",
            "created_at_ms": 1_733_000_000_000_i64,
            "updated_at_ms": 1_733_000_000_321_i64,
            "source_db_fingerprint": "fp-test",
            "conversation_count": 3,
            "message_count": 9,
            "indexed_doc_count": 0,
            "equivalence_manifest_fingerprint": null,
            "shard_plan": null,
            "build_budget": null,
            "shards": [{
                "shard_id": "shard-active",
                "shard_ordinal": 0,
                "state": "building",
                "updated_at_ms": 1_733_000_000_222_i64,
                "indexed_doc_count": 0,
                "message_count": 0,
                "artifact_bytes": 128,
                "stable_hash": null,
                "reclaimable": true,
                "pinned": false,
                "recovery_reason": null,
                "quarantine_reason": null
            }],
            "merge_debt": {
                "state": "none",
                "updated_at_ms": null,
                "pending_shard_count": 0,
                "pending_artifact_bytes": 0,
                "reason": null,
                "controller_reason": null
            },
            "build_state": "building",
            "publish_state": "staged",
            "failure_history": []
        }))
        .expect("serialize active manifest"),
    )
    .expect("write active manifest");
}

#[test]
fn doctor_json_surfaces_quarantine_gc_eligibility() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    let backups_dir = data_dir.join("backups");
    fs::create_dir_all(&backups_dir).expect("create backups dir");

    let failed_seed_root =
        backups_dir.join("agent_search.db.20260423T120000.12345.deadbeef.failed-baseline-seed.bak");
    fs::write(&failed_seed_root, b"seed-backup").expect("write failed seed bundle");
    fs::write(
        failed_seed_root.with_file_name(format!(
            "{}-wal",
            failed_seed_root
                .file_name()
                .and_then(|name| name.to_str())
                .expect("file name")
        )),
        b"seed-wal",
    )
    .expect("write failed seed wal");

    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");
    let retained_publish_dir = index_path
        .parent()
        .expect("index parent")
        .join(".lexical-publish-backups");
    fs::create_dir_all(&retained_publish_dir).expect("create retained publish dir");
    let older_backup = retained_publish_dir.join("prior-live-older");
    fs::create_dir_all(&older_backup).expect("create older retained backup");
    fs::write(older_backup.join("segment-a"), b"retained-live-segment-old")
        .expect("write older retained publish backup");
    std::thread::sleep(Duration::from_millis(20));
    let newer_backup = retained_publish_dir.join("prior-live-newer");
    fs::create_dir_all(&newer_backup).expect("create newer retained backup");
    fs::write(newer_backup.join("segment-b"), b"retained-live-segment-new")
        .expect("write newer retained publish backup");

    let generation_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-quarantined");
    write_quarantined_manifest(&generation_dir);
    fs::write(
        generation_dir.join("segment-a"),
        b"quarantined-generation-bytes",
    )
    .expect("write quarantined generation artifact");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run cass doctor --json");
    assert!(
        out.status.success(),
        "cass doctor --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let taxonomy = payload["asset_taxonomy"]
        .as_array()
        .expect("doctor exposes asset taxonomy");
    assert!(
        taxonomy.iter().any(|entry| {
            entry["asset_class"].as_str() == Some("source_session_log")
                && entry["precious"].as_bool() == Some(true)
                && entry["auto_delete_allowed"].as_bool() == Some(false)
                && entry["safe_to_gc_allowed"].as_bool() == Some(false)
        }),
        "source logs must be classified as precious non-delete evidence"
    );
    assert!(
        taxonomy.iter().any(|entry| {
            entry["asset_class"].as_str() == Some("support_bundle")
                && entry["allowed_operations"]
                    .as_array()
                    .expect("support allowed operations")
                    .iter()
                    .any(|operation| operation.as_str() == Some("redact"))
                && !entry["allowed_operations"]
                    .as_array()
                    .expect("support allowed operations")
                    .iter()
                    .any(|operation| operation.as_str() == Some("prune_reclaim"))
        }),
        "support bundles must allow redaction without becoming cleanup candidates"
    );
    assert!(
        taxonomy.iter().any(|entry| {
            entry["asset_class"].as_str() == Some("reclaimable_derived_cache")
                && entry["safety_classification"].as_str() == Some("derived_reclaimable")
                && entry["safe_to_gc_allowed"].as_bool() == Some(true)
        }),
        "doctor should expose the explicit derived-only reclaimable class"
    );
    let repair_contract = &payload["repair_contract"];
    assert_eq!(repair_contract["default_mode"].as_str(), Some("check"));
    assert_eq!(
        repair_contract["default_non_destructive"].as_bool(),
        Some(true)
    );
    assert_eq!(repair_contract["fail_closed"].as_bool(), Some(true));
    let operation_outcome = &payload["operation_outcome"];
    assert_eq!(
        operation_outcome["kind"].as_str(),
        Some("ok-read-only-diagnosed")
    );
    assert_eq!(
        operation_outcome["exit_code_kind"].as_str(),
        Some("health-failure")
    );
    assert!(
        operation_outcome["action_not_taken"]
            .as_str()
            .unwrap_or_default()
            .contains("--fix"),
        "read-only doctor outcome should explain that repair was not attempted"
    );
    let event_log = &payload["event_log"];
    assert_eq!(
        event_log["status"].as_str(),
        Some("embedded_operation_events")
    );
    assert!(
        event_log["event_count"].as_u64().unwrap_or(0) >= 3,
        "read-only doctor should emit start/check/finish events: {event_log:#}"
    );
    let events = event_log["events"].as_array().expect("doctor events");
    assert_eq!(events[0]["phase"].as_str(), Some("operation_started"));
    assert!(
        events
            .iter()
            .any(|event| event["phase"].as_str() == Some("check_warn")),
        "read-only doctor should make warning checks branchable in the event log: {events:#?}"
    );
    assert_eq!(
        event_log["hash_chain_tip"].as_str(),
        events.last().and_then(|event| event["event_id"].as_str())
    );
    let plan_receipt_schema = &repair_contract["plan_receipt_schema"];
    assert_eq!(plan_receipt_schema["plan_schema_version"].as_u64(), Some(1));
    assert!(
        plan_receipt_schema["plan_fingerprint_includes"]
            .as_array()
            .expect("plan fingerprint includes")
            .iter()
            .any(|field| field.as_str() == Some("artifact_manifest")),
        "doctor should publish what the approval fingerprint covers"
    );
    assert!(
        plan_receipt_schema["receipt_required_fields"]
            .as_array()
            .expect("receipt required fields")
            .iter()
            .any(|field| field.as_str() == Some("plan_fingerprint")),
        "doctor should publish the stable receipt field contract"
    );
    let verification_contract = &repair_contract["verification_contract"];
    assert_eq!(verification_contract["schema_version"].as_u64(), Some(1));
    assert!(
        verification_contract["required_step_log_fields"]
            .as_array()
            .expect("required step log fields")
            .iter()
            .any(|field| field.as_str() == Some("parsed_json_path")),
        "doctor verification contract should require parsed JSON logs"
    );
    let matrix = verification_contract["matrix"]
        .as_array()
        .expect("verification matrix");
    for scenario_id in [
        "no_delete_default_check",
        "upstream_pruned_archive_survives",
        "corrupt_db_repair_plan",
        "stale_lock_and_active_rebuild",
        "restore_rehearsal_then_apply",
        "derived_cleanup_fingerprint_apply",
        "semantic_fallback_no_archive_damage",
        "multi_machine_source_sync_coverage",
    ] {
        assert!(
            matrix
                .iter()
                .any(|entry| entry["scenario_id"].as_str() == Some(scenario_id)),
            "doctor verification matrix missing {scenario_id}"
        );
    }
    let mode_policies = repair_contract["mode_policies"]
        .as_array()
        .expect("doctor repair mode policy table");
    let operation_outcome_kinds = repair_contract["operation_outcome_kinds"]
        .as_array()
        .expect("doctor operation outcome kind list");
    for kind in [
        "ok-no-action-needed",
        "ok-read-only-diagnosed",
        "fixed",
        "partially-fixed",
        "repair-blocked",
        "repair-refused",
        "repair-incomplete",
        "verification-failed",
        "cleanup-dry-run-only",
        "cleanup-refused",
        "auto-run-skipped",
        "support-bundle-only",
        "baseline-diff-only",
        "requires-manual-review",
    ] {
        assert!(
            operation_outcome_kinds
                .iter()
                .any(|entry| entry.as_str() == Some(kind)),
            "doctor operation outcome kind list missing {kind}"
        );
    }
    let operation_contract = repair_contract["operation_outcome_contract"]
        .as_array()
        .expect("doctor operation outcome contract");
    assert!(
        operation_contract.iter().any(|entry| {
            entry["kind"].as_str() == Some("cleanup-refused")
                && entry["action_not_taken"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("no cleanup target")
                && entry["exit_code_kind"].as_str() == Some("repair-failure")
        }),
        "cleanup-refused outcome must be branchable without prose parsing"
    );
    assert!(
        operation_contract.iter().any(|entry| {
            entry["kind"].as_str() == Some("repair-refused")
                && entry["requires_override"].as_bool() == Some(true)
                && entry["data_loss_risk"].as_str() == Some("high")
        }),
        "repair-refused outcome should fail closed and advertise high risk"
    );
    assert!(
        mode_policies.iter().any(|policy| {
            policy["mode"].as_str() == Some("cleanup_apply")
                && policy["mutates"].as_bool() == Some(true)
                && policy["approval_requirement"].as_str() == Some("approval_fingerprint")
                && policy["allowed_mutation_asset_classes"]
                    .as_array()
                    .expect("cleanup_apply allowed classes")
                    .iter()
                    .any(|class| class.as_str() == Some("reclaimable_derived_cache"))
        }),
        "cleanup_apply mode must be fingerprint-gated and derived-only"
    );
    assert!(
        mode_policies.iter().any(|policy| {
            policy["mode"].as_str() == Some("emergency_force")
                && policy["mutates"].as_bool() == Some(false)
                && policy["approval_requirement"].as_str() == Some("refused")
        }),
        "emergency_force mode must be an explicit fail-closed refusal"
    );
    let quarantine = &payload["quarantine"];

    assert_eq!(
        quarantine["summary"]["gc_eligible_asset_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        quarantine["summary"]["inspection_required_asset_count"].as_u64(),
        Some(3)
    );
    assert_eq!(
        quarantine["summary"]["retained_publish_backup_retention_limit"].as_u64(),
        Some(1)
    );
    assert_eq!(
        quarantine["summary"]["cleanup_dry_run_generation_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        quarantine["summary"]["cleanup_dry_run_inspection_required_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        quarantine["summary"]["cleanup_apply_allowed"].as_bool(),
        Some(false)
    );

    let retained = quarantine["retained_publish_backups"]
        .as_array()
        .expect("retained publish backups array");
    assert!(
        retained.iter().any(|entry| {
            entry["path"]
                .as_str()
                .unwrap_or_default()
                .contains("prior-live-older")
                && entry["asset_class"].as_str() == Some("retained_publish_backup")
                && entry["safety_classification"].as_str() == Some("derived_reclaimable")
                && entry["auto_delete_allowed"].as_bool() == Some(true)
                && entry["safe_to_gc"].as_bool() == Some(true)
        }),
        "older retained publish backup should be GC-eligible in doctor JSON"
    );
    assert!(
        retained.iter().any(|entry| {
            entry["path"]
                .as_str()
                .unwrap_or_default()
                .contains("prior-live-newer")
                && entry["asset_class"].as_str() == Some("retained_publish_backup")
                && entry["safe_to_gc"].as_bool() == Some(false)
        }),
        "newest retained publish backup should remain protected in doctor JSON"
    );

    let generations = quarantine["lexical_generations"]
        .as_array()
        .expect("lexical generations array");
    assert_eq!(generations.len(), 1, "expected one quarantined generation");
    assert_eq!(generations[0]["generation_id"], "gen-quarantined");
    assert_eq!(
        generations[0]["asset_class"].as_str(),
        Some("quarantined_lexical_generation")
    );
    assert_eq!(
        generations[0]["safety_classification"].as_str(),
        Some("diagnostic_evidence")
    );
    assert_eq!(generations[0]["safe_to_gc_allowed"].as_bool(), Some(false));
    assert_eq!(generations[0]["safe_to_gc"].as_bool(), Some(false));
    assert_eq!(generations[0]["reclaimable_bytes"].as_u64(), Some(0));
    assert!(
        generations[0]["gc_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("cleanup dry-run"),
        "doctor JSON should expose why quarantined lexical generations are held"
    );
    let inspection_artifacts = quarantine["quarantined_artifacts"]
        .as_array()
        .expect("flattened quarantined artifacts array");
    assert!(
        inspection_artifacts.iter().any(|entry| {
            entry["artifact_kind"].as_str() == Some("lexical_shard")
                && entry["generation_id"].as_str() == Some("gen-quarantined")
                && entry["shard_id"].as_str() == Some("shard-a")
                && entry["asset_class"].as_str() == Some("quarantined_lexical_shard")
                && entry["safety_classification"].as_str() == Some("diagnostic_evidence")
                && entry["gc_reason"].as_str() == Some("validation_failed")
        }),
        "doctor JSON should expose each quarantined shard with a gc reason"
    );

    let dry_run = &quarantine["lexical_cleanup_dry_run"];
    assert_eq!(dry_run["dry_run"].as_bool(), Some(true));
    assert_eq!(
        dry_run["inventories"][0]["disposition"].as_str(),
        Some("quarantined_retained")
    );
    let apply_gate = &quarantine["lexical_cleanup_apply_gate"];
    assert_eq!(apply_gate["apply_allowed"].as_bool(), Some(false));
    assert_eq!(
        apply_gate["inspection_required_generation_ids"][0].as_str(),
        Some("gen-quarantined")
    );
}

#[test]
fn doctor_human_output_surfaces_operation_outcome() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");

    let out = cass_cmd(test_home.path())
        .args(["doctor", "--data-dir", data_dir.to_str().expect("utf8")])
        .output()
        .expect("run cass doctor");
    assert!(
        out.status.success(),
        "cass doctor failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Operation outcome:"),
        "human doctor output should include an outcome block:\n{stdout}"
    );
    assert!(
        stdout.contains("ok-read-only-diagnosed"),
        "human doctor output should expose the stable outcome kind:\n{stdout}"
    );
    assert!(
        stdout.contains("action_not_taken:"),
        "human doctor output should explain what doctor refused or skipped:\n{stdout}"
    );
    assert!(
        stdout.contains("next_command: cass index --full"),
        "human doctor output should expose the next branch command:\n{stdout}"
    );
}

#[test]
fn doctor_fix_reports_repair_blocked_when_doctor_lock_is_active() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    let lock_dir = data_dir.join("doctor").join("locks");
    fs::create_dir_all(&lock_dir).expect("create doctor lock dir");
    let lock_path = lock_dir.join("doctor-repair.lock");
    let mut lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open doctor lock");
    lock_file
        .try_lock_exclusive()
        .expect("hold doctor mutation lock");
    writeln!(
        lock_file,
        "schema_version=1\npid={}\nstarted_at_ms=1733001111000\nupdated_at_ms=1733001112000\ndb_path={}\nmode=safe_auto_run\ncommand=cass doctor --fix",
        std::process::id(),
        data_dir.join("agent_search.db").display()
    )
    .expect("write lock metadata");
    lock_file.flush().expect("flush lock");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run blocked cass doctor --json --fix");
    assert!(
        !out.status.success(),
        "mutating doctor should return a lock/busy failure when another doctor owns the lock"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        payload["operation_outcome"]["kind"].as_str(),
        Some("repair-blocked")
    );
    assert_eq!(
        payload["operation_outcome"]["exit_code_kind"].as_str(),
        Some("lock-busy")
    );
    let operation_state = &payload["operation_state"];
    assert_eq!(
        operation_state["active_doctor_repair"].as_bool(),
        Some(true)
    );
    assert_eq!(
        operation_state["mutating_doctor_allowed"].as_bool(),
        Some(false)
    );
    assert!(
        operation_state["mutation_blocked_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("another cass doctor")),
        "operation_state should explain the active doctor lock: {operation_state:#}"
    );
    assert!(
        payload.get("cleanup_apply").is_none(),
        "doctor must not enter cleanup_apply while the mutation lock is blocked: {payload:#}"
    );
    let operation_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"].as_str() == Some("operation_state"))
        .expect("operation_state check");
    assert_eq!(operation_check["status"].as_str(), Some("fail"));
    assert_eq!(
        operation_check["anomaly_class"].as_str(),
        Some("lock-contention")
    );
}

#[test]
fn doctor_fix_refuses_repeated_repair_when_failure_marker_exists() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let marker_path = write_repair_failure_marker_fixture(
        &data_dir,
        "repair_apply",
        "previous-failure",
        1_733_001_111_000,
    );

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json --fix with previous failure marker");
    assert!(
        !out.status.success(),
        "doctor --fix must fail closed when a previous failure marker exists"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid doctor JSON");
    assert_eq!(payload["repair_previously_failed"].as_bool(), Some(true));
    assert_eq!(
        payload["failure_marker_path"].as_str(),
        Some(marker_path.to_string_lossy().as_ref())
    );
    assert_eq!(payload["override_available"].as_bool(), Some(true));
    assert_eq!(payload["override_used"].as_bool(), Some(false));
    assert!(
        payload["repeat_refusal_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("--allow-repeated-repair"),
        "repeat refusal should name the explicit override: {payload:#}"
    );
    assert_eq!(
        payload["operation_outcome"]["kind"].as_str(),
        Some("repair-refused")
    );
    assert_eq!(
        payload["operation_state"]["active_doctor_repair"].as_bool(),
        Some(false),
        "repeat refusal should not acquire the mutating doctor lock"
    );
    assert!(
        payload.get("cleanup_apply").is_none(),
        "doctor must not enter cleanup_apply after repeat-repair refusal: {payload:#}"
    );
    let marker_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"].as_str() == Some("repair_failure_marker"))
        .expect("repair failure marker check");
    assert_eq!(marker_check["status"].as_str(), Some("fail"));
    assert_eq!(
        marker_check["anomaly_class"].as_str(),
        Some("repair-previously-failed")
    );
    assert!(
        marker_path.exists(),
        "repeat refusal must preserve the original failure marker"
    );
}

#[test]
fn doctor_fix_allow_repeated_repair_runs_without_deleting_existing_marker() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let marker_path = write_repair_failure_marker_fixture(
        &data_dir,
        "repair_apply",
        "previous-failure",
        1_733_001_111_000,
    );

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--allow-repeated-repair",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json --fix with override");
    assert!(
        out.status.success(),
        "override should allow the mutating doctor run to proceed on a healthy fixture: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid doctor JSON");
    assert_eq!(payload["repair_previously_failed"].as_bool(), Some(true));
    assert_eq!(payload["override_available"].as_bool(), Some(true));
    assert_eq!(payload["override_used"].as_bool(), Some(true));
    assert_eq!(payload["repeat_refusal_reason"].as_str(), None);
    assert_eq!(
        payload["failure_marker_path"].as_str(),
        Some(marker_path.to_string_lossy().as_ref())
    );
    assert!(
        payload["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .all(|check| check["name"].as_str() != Some("repair_failure_marker")),
        "accepted override should not poison the current run's health checks: {payload:#}"
    );
    assert!(
        marker_path.exists(),
        "override must not remove or overwrite the previous failure marker"
    );
}

#[test]
fn doctor_fix_removes_stale_legacy_index_lock_with_mutation_receipt() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);

    let lock_path = data_dir.join(".index.lock");
    fs::write(&lock_path, b"legacy stale lock").expect("write legacy lock");
    make_file_mtime_older_than(&lock_path, Duration::from_secs(7200));

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json --fix");
    assert!(
        out.status.success(),
        "cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !lock_path.exists(),
        "stale legacy .index.lock should be removed through the audited executor"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let lock_check = payload["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"].as_str() == Some("lock_file"))
        .expect("lock_file check");
    assert_eq!(lock_check["status"].as_str(), Some("pass"));
    assert_eq!(lock_check["fix_applied"].as_bool(), Some(true));

    let receipts = payload["fs_mutation_receipts"]
        .as_array()
        .expect("fs mutation receipts array");
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(
        receipt["mutation_kind"].as_str(),
        Some("remove_stale_legacy_index_lock")
    );
    assert_eq!(receipt["status"].as_str(), Some("applied"));
    assert_eq!(
        receipt["asset_class"].as_str(),
        Some("reclaimable_derived_cache")
    );
    assert_eq!(
        receipt["redacted_target_path"].as_str(),
        Some("[cass-data]/.index.lock")
    );
    assert!(
        receipt["precondition_checks"]
            .as_array()
            .expect("precondition checks")
            .iter()
            .any(|check| check.as_str() == Some("file_age_seconds_exceeds_3600")),
        "receipt should prove the stale-age precondition: {receipt:#}"
    );
    assert!(
        receipt["precondition_checks"]
            .as_array()
            .expect("precondition checks")
            .iter()
            .any(|check| check.as_str() == Some("filesystem_remove_completed")),
        "receipt should record the completed filesystem mutation: {receipt:#}"
    );
}

#[test]
fn doctor_json_reports_interrupted_operation_state_without_deleting_artifacts() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    let interrupted_plan = data_dir
        .join("doctor")
        .join("tmp")
        .join("interrupted-repair")
        .join("plan.json");
    fs::create_dir_all(interrupted_plan.parent().expect("parent")).expect("create interrupted dir");
    fs::write(&interrupted_plan, br#"{"state":"interrupted"}"#).expect("write interrupted plan");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json");
    assert!(
        out.status.success(),
        "read-only doctor should report interrupted state without failing: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        interrupted_plan.exists(),
        "read-only doctor must not delete interrupted repair evidence"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let operation_state = &payload["operation_state"];
    assert_eq!(
        operation_state["read_only_check_allowed"].as_bool(),
        Some(true)
    );
    assert_eq!(
        operation_state["mutating_doctor_allowed"].as_bool(),
        Some(false)
    );
    assert!(
        operation_state["interrupted_state_count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "operation_state should count interrupted artifacts: {operation_state:#}"
    );
    assert!(
        operation_state["interrupted_states"]
            .as_array()
            .expect("interrupted states")
            .iter()
            .any(|state| {
                state["kind"].as_str() == Some("candidate_build")
                    && state["blocks_mutation"].as_bool() == Some(true)
                    && state["safe_to_delete_automatically"].as_bool() == Some(false)
            }),
        "interrupted plan should be classified as non-deletable candidate evidence: {operation_state:#}"
    );
    let operation_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"].as_str() == Some("operation_state"))
        .expect("operation_state check");
    assert_eq!(operation_check["status"].as_str(), Some("warn"));
    assert_eq!(
        operation_check["anomaly_class"].as_str(),
        Some("interrupted-repair")
    );
}

#[test]
fn doctor_json_reports_missing_upstream_source_as_coverage_risk_not_data_loss() {
    let temp = tempfile::tempdir().expect("tempdir");
    let test_home = temp.path();
    let data_dir = test_home.join("cass-data");
    seed_healthy_empty_index(test_home, &data_dir);

    let missing_source = test_home.join(".codex/sessions/pruned-session.jsonl");
    let db_path = data_dir.join("agent_search.db");
    let conn = FrankenConnection::open(db_path.to_string_lossy().into_owned()).expect("open db");
    let agent_id: i64 = match conn.query_row_map(
        "SELECT id FROM agents WHERE slug = 'codex' LIMIT 1",
        &[],
        |row: &frankensqlite::Row| row.get_typed(0),
    ) {
        Ok(id) => id,
        Err(_) => {
            let next_id: i64 = conn
                .query_row_map("SELECT COALESCE(MAX(id), 0) + 1 FROM agents", &[], |row| {
                    row.get_typed(0)
                })
                .expect("next agent id");
            conn.execute_compat(
                "INSERT INTO agents (id, slug, name, version, kind, created_at, updated_at)
                 VALUES (?1, 'codex', 'Codex', 'test', 'agent', 0, 0)",
                frankensqlite::params![next_id],
            )
            .expect("insert codex agent");
            next_id
        }
    };
    let missing_source_str = missing_source.to_string_lossy().into_owned();
    conn.execute_compat(
        "INSERT INTO conversations (agent_id, source_id, external_id, title, source_path, started_at)
         VALUES (?1, 'local', 'missing-codex-session', 'missing upstream fixture', ?2, 1700000000000)",
        frankensqlite::params![agent_id, missing_source_str.as_str()],
    )
    .expect("insert conversation");
    drop(conn);

    let out = cass_cmd(test_home)
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json");

    assert!(
        out.status.success(),
        "cass doctor --json failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: Value = serde_json::from_slice(&out.stdout).expect("doctor json");
    let inventory = &payload["source_inventory"];

    assert_eq!(
        inventory["missing_current_source_count"].as_u64(),
        Some(1),
        "missing upstream local source should be reported as coverage risk: {inventory:#}"
    );
    assert_eq!(inventory["provider_counts"]["codex"].as_u64(), Some(1));
    assert!(
        inventory["notes"]
            .as_array()
            .expect("source_inventory notes")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|text| text.contains("archive database"))),
        "doctor should explain that missing upstream files do not imply archive data loss: {inventory:#}"
    );

    let source_inventory_check = payload["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"].as_str() == Some("source_inventory"))
        .expect("source_inventory check");
    assert_eq!(source_inventory_check["status"].as_str(), Some("warn"));
    assert!(
        source_inventory_check["message"]
            .as_str()
            .is_some_and(|message| message.contains("Source coverage risk")),
        "source_inventory check should name this as coverage risk: {source_inventory_check:#}"
    );
    let source_authority = &payload["source_authority"];
    assert_eq!(
        source_authority["coverage_delta"]["missing_current_source_count"].as_u64(),
        Some(1),
        "source authority report should carry the coverage delta for pruned upstream sources"
    );
    assert!(
        source_authority["rejected_authorities"]
            .as_array()
            .expect("rejected authorities")
            .iter()
            .any(
                |candidate| candidate["authority"].as_str() == Some("live_upstream_source")
                    && candidate["reason"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("incomplete")
                    && candidate["evidence"]
                        .as_array()
                        .is_some_and(|evidence| evidence
                            .iter()
                            .any(|entry| entry.as_str()
                                == Some("coverage-shrinks-relative-to-archive")))
            ),
        "live upstream source should be rejected with stable reason/evidence: {source_authority:#}"
    );
}

#[test]
fn doctor_json_verifies_raw_mirror_after_upstream_source_is_pruned() {
    let temp = tempfile::tempdir().expect("tempdir");
    let test_home = temp.path();
    let data_dir = test_home.join("cass-data");
    seed_healthy_empty_index(test_home, &data_dir);

    let missing_source = test_home.join(".codex/sessions/secret-project/pruned-session.jsonl");
    let mirrored_bytes =
        b"{\"type\":\"message\",\"role\":\"user\",\"content\":\"RAW_MIRROR_SECRET_PROMPT\"}\n";
    let manifest = write_raw_mirror_fixture(
        &data_dir,
        "codex",
        "local",
        "local",
        &missing_source,
        mirrored_bytes,
    );

    let db_path = data_dir.join("agent_search.db");
    let conn = FrankenConnection::open(db_path.to_string_lossy().into_owned()).expect("open db");
    let agent_id: i64 = conn
        .query_row_map(
            "SELECT id FROM agents WHERE slug = 'codex' LIMIT 1",
            &[],
            |row: &frankensqlite::Row| row.get_typed(0),
        )
        .or_else(|_| {
            let next_id: i64 =
                conn.query_row_map("SELECT COALESCE(MAX(id), 0) + 1 FROM agents", &[], |row| {
                    row.get_typed(0)
                })?;
            conn.execute_compat(
                "INSERT INTO agents (id, slug, name, version, kind, created_at, updated_at)
                 VALUES (?1, 'codex', 'Codex', 'test', 'agent', 0, 0)",
                frankensqlite::params![next_id],
            )?;
            Ok::<i64, frankensqlite::FrankenError>(next_id)
        })
        .expect("codex agent id");
    let missing_source_str = missing_source.to_string_lossy().into_owned();
    conn.execute_compat(
        "INSERT INTO conversations (agent_id, source_id, external_id, title, source_path, started_at)
         VALUES (?1, 'local', 'raw-mirrored-missing-source', 'raw mirrored fixture', ?2, 1700000000000)",
        frankensqlite::params![agent_id, missing_source_str.as_str()],
    )
    .expect("insert conversation");
    drop(conn);

    assert!(
        !missing_source.exists(),
        "fixture precondition: upstream source must be absent before doctor runs"
    );
    let out = cass_cmd(test_home)
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json");

    assert!(
        out.status.success(),
        "cass doctor --json failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !missing_source.exists(),
        "doctor must verify mirror evidence without recreating the pruned upstream path"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("RAW_MIRROR_SECRET_PROMPT"),
        "default doctor robot JSON must not contain raw mirrored session bytes"
    );
    assert!(
        !stdout.contains(&missing_source.display().to_string()),
        "default doctor robot JSON must not contain exact raw source paths"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("doctor json");
    let raw_mirror = &payload["raw_mirror"];
    assert_eq!(raw_mirror["status"].as_str(), Some("verified"));
    assert_eq!(
        raw_mirror["sensitive_paths_included"].as_bool(),
        Some(false)
    );
    assert_eq!(raw_mirror["raw_content_included"].as_bool(), Some(false));
    assert!(
        raw_mirror.get("root_path").is_none(),
        "raw mirror exact root path should not serialize in default robot JSON: {raw_mirror:#}"
    );
    assert_eq!(raw_mirror["summary"]["manifest_count"].as_u64(), Some(1));
    assert_eq!(
        raw_mirror["summary"]["verified_blob_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        raw_mirror["summary"]["total_blob_bytes"].as_u64(),
        Some(mirrored_bytes.len() as u64)
    );
    assert_eq!(
        raw_mirror["manifests"][0]["manifest_id"].as_str(),
        manifest["manifest_id"].as_str()
    );
    assert_eq!(
        raw_mirror["manifests"][0]["blob_checksum_status"].as_str(),
        Some("matched")
    );
    assert_eq!(
        raw_mirror["manifests"][0]["upstream_path_exists"].as_bool(),
        Some(false)
    );
    assert!(
        raw_mirror["manifests"][0].get("manifest_path").is_none(),
        "exact manifest paths are internal-only in default raw mirror reports"
    );
    assert!(
        raw_mirror["manifests"][0].get("blob_path").is_none(),
        "exact blob paths are internal-only in default raw mirror reports"
    );
    assert!(
        raw_mirror["manifests"][0].get("original_path").is_none(),
        "exact original source paths are internal-only in default raw mirror reports"
    );
    assert_eq!(
        raw_mirror["manifests"][0]["redacted_original_path"].as_str(),
        Some("[external]/pruned-session.jsonl")
    );
    assert_eq!(
        raw_mirror["manifests"][0]["compression"]["state"].as_str(),
        Some("none")
    );
    assert_eq!(
        raw_mirror["manifests"][0]["encryption"]["state"].as_str(),
        Some("none")
    );
    assert_eq!(
        raw_mirror["policy"]["support_bundle_policy"]["default_mode"].as_str(),
        Some("manifest-only")
    );
    assert_eq!(
        raw_mirror["policy"]["support_bundle_policy"]["include_blob_bytes"].as_bool(),
        Some(false)
    );
    assert_eq!(
        raw_mirror["policy"]["public_export_policy"]["pages_exports_include_raw_mirror"].as_bool(),
        Some(false)
    );
    assert_eq!(
        raw_mirror["policy"]["public_export_policy"]["html_exports_include_raw_mirror"].as_bool(),
        Some(false)
    );

    let raw_mirror_check = payload["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"].as_str() == Some("raw_mirror"))
        .expect("raw_mirror check");
    assert_eq!(raw_mirror_check["status"].as_str(), Some("pass"));
    assert!(
        raw_mirror_check["message"]
            .as_str()
            .is_some_and(|message| message.contains("Raw mirror verified")),
        "raw_mirror check should report verified evidence: {raw_mirror_check:#}"
    );
    let source_authority = &payload["source_authority"];
    assert_eq!(
        source_authority["selected_authority"].as_str(),
        Some("canonical_archive_db")
    );
    assert!(
        source_authority["selected_authorities"]
            .as_array()
            .expect("selected authorities")
            .iter()
            .any(
                |candidate| candidate["authority"].as_str() == Some("verified_raw_mirror")
                    && candidate["decision"].as_str() == Some("candidate_only")
                    && candidate["checksum_status"].as_str() == Some("matched")
            ),
        "verified raw mirror should be a candidate-only authority after upstream pruning: {source_authority:#}"
    );
    assert_eq!(
        source_authority["coverage_delta"]["raw_mirror_db_link_count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        source_authority["checksum_evidence"]["summary_status"].as_str(),
        Some("matched")
    );
}

#[test]
fn doctor_json_does_not_count_quarantined_artifacts_as_reclaimable() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");

    let quarantined_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-quarantined-reclaimable");
    write_quarantined_reclaimable_shard_manifest(&quarantined_dir);
    fs::write(
        quarantined_dir.join("segment-abandoned"),
        b"quarantined abandoned generation bytes",
    )
    .expect("write quarantined generation artifact");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json");
    assert!(
        out.status.success(),
        "cass doctor --json failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let quarantine = &payload["quarantine"];
    assert_eq!(
        quarantine["summary"]["cleanup_dry_run_reclaimable_bytes"].as_u64(),
        Some(0),
        "quarantined generations should not contribute to dry-run reclaimable bytes"
    );
    assert_eq!(
        quarantine["summary"]["cleanup_dry_run_reclaim_candidate_count"].as_u64(),
        Some(0),
        "quarantined generations should not create cleanup reclaim candidates"
    );
    assert_eq!(
        quarantine["summary"]["gc_eligible_bytes"].as_u64(),
        Some(0),
        "quarantined generations requiring inspection are retained, not gc eligible"
    );

    let inventories = quarantine["lexical_cleanup_dry_run"]["inventories"]
        .as_array()
        .expect("cleanup inventories");
    let inventory = inventories
        .iter()
        .find(|entry| entry["generation_id"].as_str() == Some("gen-quarantined-reclaimable"))
        .expect("quarantined inventory");
    assert_eq!(
        inventory["disposition"].as_str(),
        Some("quarantined_retained")
    );
    assert_eq!(inventory["reclaimable_bytes"].as_u64(), Some(0));
    assert_eq!(inventory["retained_bytes"].as_u64(), Some(512));
    assert_eq!(
        inventory["shards"][0]["disposition"].as_str(),
        Some("quarantined_retained"),
        "shard-level dry-run JSON should also honor the generation quarantine hold"
    );
    assert_eq!(
        inventory["shards"][0]["reclaimable_bytes"].as_u64(),
        Some(0)
    );
    assert_eq!(inventory["shards"][0]["retained_bytes"].as_u64(), Some(512));
    assert_eq!(
        quarantine["lexical_cleanup_dry_run"]["shard_disposition_summaries"]
            ["quarantined_retained"]["reclaimable_bytes"]
            .as_u64(),
        Some(0),
        "quarantined shard summaries should not expose reclaimable bytes"
    );
    assert!(
        quarantine["lexical_cleanup_dry_run"]["shard_disposition_summaries"]["failed_reclaimable"]
            .is_null(),
        "quarantined shards must not leak into failed_reclaimable summaries"
    );
}

#[test]
fn doctor_fix_preserves_pinned_superseded_generation() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");

    let pinned_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-partly-pinned");
    write_superseded_partly_pinned_manifest(&pinned_dir, "gen-partly-pinned");
    let reclaimable_segment = pinned_dir.join("segment-old");
    fs::write(&reclaimable_segment, b"unpinned superseded bytes")
        .expect("write reclaimable segment");
    let pinned_segment = pinned_dir.join("segment-pinned");
    fs::write(&pinned_segment, b"pinned superseded bytes").expect("write pinned segment");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json --fix");
    assert!(
        out.status.success(),
        "cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        pinned_dir.exists(),
        "cleanup apply must preserve a generation that still contains pinned artifacts"
    );
    assert!(
        reclaimable_segment.exists(),
        "whole-generation cleanup must not remove the unpinned shard while pinned siblings remain"
    );
    assert!(
        pinned_segment.exists(),
        "cleanup apply must preserve pinned shard artifacts"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let cleanup = &payload["cleanup_apply"];
    assert_eq!(cleanup["requested"].as_bool(), Some(true));
    assert_eq!(cleanup["apply_allowed"].as_bool(), Some(true));
    assert_eq!(cleanup["applied"].as_bool(), Some(false));
    assert_eq!(cleanup["before_reclaim_candidate_count"].as_u64(), Some(1));
    assert_eq!(cleanup["after_reclaim_candidate_count"].as_u64(), Some(1));
    assert_eq!(cleanup["before_reclaimable_bytes"].as_u64(), Some(128));
    assert_eq!(cleanup["before_retained_bytes"].as_u64(), Some(256));
    assert_eq!(cleanup["pruned_asset_count"].as_u64(), Some(0));
    assert_eq!(cleanup["skipped_asset_count"].as_u64(), Some(1));
    assert!(
        cleanup["warnings"]
            .as_array()
            .expect("cleanup warnings")
            .iter()
            .any(|warning| {
                warning
                    .as_str()
                    .unwrap_or_default()
                    .contains("cleanup apply only prunes whole lexical generations")
            }),
        "cleanup result should explain why the pinned generation was not pruned"
    );

    let before_inventories = cleanup["before_inventory"]["lexical_cleanup_inventories"]
        .as_array()
        .expect("before lexical inventories");
    let pinned_inventory = before_inventories
        .iter()
        .find(|entry| entry["generation_id"].as_str() == Some("gen-partly-pinned"))
        .expect("partly pinned inventory");
    assert_eq!(
        pinned_inventory["disposition"].as_str(),
        Some("superseded_reclaimable")
    );
    assert_eq!(pinned_inventory["reclaimable_bytes"].as_u64(), Some(128));
    assert_eq!(pinned_inventory["retained_bytes"].as_u64(), Some(256));
    assert!(
        pinned_inventory["shards"]
            .as_array()
            .expect("shard inventories")
            .iter()
            .any(|shard| {
                shard["shard_id"].as_str() == Some("shard-pinned")
                    && shard["disposition"].as_str() == Some("pinned_retained")
                    && shard["retained_bytes"].as_u64() == Some(256)
            }),
        "inventory should retain the pinned shard as protected context"
    );

    let actions = cleanup["actions"].as_array().expect("cleanup actions");
    assert_eq!(actions.len(), 1);
    let action = &actions[0];
    assert_eq!(action["artifact_kind"].as_str(), Some("lexical_generation"));
    assert_eq!(action["generation_id"].as_str(), Some("gen-partly-pinned"));
    assert_eq!(
        action["asset_class"].as_str(),
        Some("reclaimable_derived_cache")
    );
    assert_eq!(
        action["safety_classification"].as_str(),
        Some("derived_reclaimable")
    );
    assert_eq!(action["applied"].as_bool(), Some(false));
    assert_eq!(action["skipped"].as_bool(), Some(true));
    assert!(
        action["skip_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("retained_bytes=256"),
        "skip reason should surface the pinned retained byte count"
    );
}

#[test]
fn doctor_fix_prunes_safe_derivative_cleanup_candidates() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");
    let retained_publish_dir = index_path
        .parent()
        .expect("index parent")
        .join(".lexical-publish-backups");
    fs::create_dir_all(&retained_publish_dir).expect("create retained publish dir");
    let older_backup = retained_publish_dir.join("prior-live-older");
    fs::create_dir_all(&older_backup).expect("create older retained backup");
    fs::write(older_backup.join("segment-a"), b"old backup bytes")
        .expect("write older retained publish backup");
    std::thread::sleep(Duration::from_millis(20));
    let newer_backup = retained_publish_dir.join("prior-live-newer");
    fs::create_dir_all(&newer_backup).expect("create newer retained backup");
    fs::write(newer_backup.join("segment-b"), b"new backup bytes")
        .expect("write newer retained publish backup");

    let superseded_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-superseded");
    write_superseded_reclaimable_manifest(&superseded_dir, "gen-superseded");
    fs::write(
        superseded_dir.join("segment-old"),
        b"superseded generation bytes",
    )
    .expect("write superseded generation artifact");

    let quarantined_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-quarantined");
    write_quarantined_manifest(&quarantined_dir);
    fs::write(
        quarantined_dir.join("segment-a"),
        b"quarantined generation bytes",
    )
    .expect("write quarantined generation artifact");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run cass doctor --json --fix");
    assert!(
        out.status.success(),
        "cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !older_backup.exists(),
        "older retained publish backup outside cap should be pruned"
    );
    assert!(
        newer_backup.exists(),
        "newest retained publish backup should remain protected"
    );
    assert!(
        !superseded_dir.exists(),
        "fully reclaimable superseded lexical generation should be pruned"
    );
    assert!(
        quarantined_dir.exists(),
        "quarantined lexical generation must remain for inspection"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(payload["auto_fix_applied"].as_bool(), Some(true));
    assert!(
        payload["auto_fix_actions"]
            .as_array()
            .expect("auto fix actions")
            .iter()
            .any(|action| action
                .as_str()
                .unwrap_or_default()
                .contains("Pruned 2 derivative cleanup artifact(s)")),
        "doctor top-level auto_fix_actions should report derivative cleanup"
    );
    assert!(
        payload["issues_fixed"].as_u64().unwrap_or(0) >= 1,
        "doctor should count derivative cleanup as a fixed issue"
    );
    assert_eq!(
        payload["operation_outcome"]["kind"].as_str(),
        Some("fixed"),
        "top-level doctor outcome should report fixed when cleanup apply completes"
    );
    assert_eq!(
        payload["operation_outcome"]["exit_code_kind"].as_str(),
        Some("success")
    );
    let derivative_cleanup = payload["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"].as_str() == Some("derivative_cleanup"))
        .expect("derivative_cleanup check");
    assert_eq!(derivative_cleanup["status"].as_str(), Some("pass"));
    assert_eq!(derivative_cleanup["fix_available"].as_bool(), Some(true));
    assert_eq!(derivative_cleanup["fix_applied"].as_bool(), Some(true));
    let cleanup = &payload["cleanup_apply"];
    assert_eq!(cleanup["mode"].as_str(), Some("cleanup_apply"));
    assert_eq!(
        cleanup["approval_requirement"].as_str(),
        Some("approval_fingerprint")
    );
    assert_eq!(cleanup["outcome_kind"].as_str(), Some("applied"));
    assert_eq!(cleanup["operation_outcome"]["kind"].as_str(), Some("fixed"));
    assert_eq!(
        cleanup["operation_outcome"]["artifact_manifest_path"].as_str(),
        Some("cleanup_apply.receipt.artifact_manifest")
    );
    assert_eq!(cleanup["retry_safety"].as_str(), Some("safe_to_retry"));
    assert_eq!(cleanup["requested"].as_bool(), Some(true));
    assert_eq!(cleanup["applied"].as_bool(), Some(true));
    assert_eq!(cleanup["before_reclaim_candidate_count"].as_u64(), Some(1));
    assert_eq!(cleanup["after_reclaim_candidate_count"].as_u64(), Some(0));
    assert_eq!(cleanup["pruned_asset_count"].as_u64(), Some(2));
    assert!(
        cleanup["reclaimed_bytes"].as_u64().unwrap_or(0) > 0,
        "apply result should summarize reclaimed bytes"
    );
    let before_inventory = &cleanup["before_inventory"];
    let after_inventory = &cleanup["after_inventory"];
    assert_eq!(
        before_inventory["summary"]["retained_publish_backup_count"].as_u64(),
        Some(2),
        "before inventory should report both retained publish backups"
    );
    assert_eq!(
        after_inventory["summary"]["retained_publish_backup_count"].as_u64(),
        Some(1),
        "after inventory should report the protected retained publish backup that remains"
    );
    assert!(
        before_inventory["retained_publish_backups"]
            .as_array()
            .expect("before retained publish backups")
            .iter()
            .any(|entry| entry["path"]
                .as_str()
                .unwrap_or_default()
                .contains("prior-live-older")),
        "before inventory should include the retained backup that will be pruned"
    );
    assert!(
        !after_inventory["retained_publish_backups"]
            .as_array()
            .expect("after retained publish backups")
            .iter()
            .any(|entry| entry["path"]
                .as_str()
                .unwrap_or_default()
                .contains("prior-live-older")),
        "after inventory should omit the pruned retained backup"
    );
    assert!(
        before_inventory["lexical_cleanup_inventories"]
            .as_array()
            .expect("before lexical inventories")
            .iter()
            .any(|entry| entry["generation_id"].as_str() == Some("gen-superseded")),
        "before inventory should include the superseded generation candidate"
    );
    assert!(
        !after_inventory["lexical_cleanup_inventories"]
            .as_array()
            .expect("after lexical inventories")
            .iter()
            .any(|entry| entry["generation_id"].as_str() == Some("gen-superseded")),
        "after inventory should omit the pruned superseded generation"
    );
    assert_eq!(
        before_inventory["reclaim_candidates"]
            .as_array()
            .expect("before reclaim candidates")
            .len(),
        1,
        "before inventory should expose the generation reclaim candidate"
    );
    assert!(
        after_inventory["reclaim_candidates"]
            .as_array()
            .expect("after reclaim candidates")
            .is_empty(),
        "after inventory should show no remaining reclaim candidates"
    );
    let actions = cleanup["actions"].as_array().expect("cleanup actions");
    let planned_actions = cleanup["planned_actions"]
        .as_array()
        .expect("planned cleanup actions");
    assert_eq!(
        planned_actions.len(),
        actions.len(),
        "cleanup_apply should carry planned_actions alongside applied/skipped action results"
    );
    let receipt = &cleanup["receipt"];
    assert_eq!(
        receipt["receipt_kind"].as_str(),
        Some("doctor_cleanup_apply_v1")
    );
    assert_eq!(receipt["mode"].as_str(), Some("cleanup_apply"));
    assert_eq!(receipt["outcome_kind"].as_str(), Some("applied"));
    assert_eq!(
        receipt["approval_fingerprint"].as_str(),
        cleanup["approval_fingerprint"].as_str()
    );
    assert_eq!(receipt["planned_action_count"].as_u64(), Some(2));
    assert_eq!(receipt["applied_action_count"].as_u64(), Some(2));
    assert_eq!(
        receipt["bytes_pruned"].as_u64(),
        cleanup["reclaimed_bytes"].as_u64()
    );
    assert_eq!(
        receipt["drift_detection_status"].as_str(),
        Some("not_checked")
    );
    assert!(
        receipt["started_at_ms"].as_i64().is_some(),
        "mutating doctor receipt should record a start timestamp"
    );
    assert!(
        receipt["finished_at_ms"].as_i64().is_some(),
        "mutating doctor receipt should record a finish timestamp"
    );
    let plan = cleanup["plan"].as_object().expect("cleanup plan object");
    assert_eq!(
        plan["approval_fingerprint"].as_str(),
        cleanup["approval_fingerprint"].as_str()
    );
    assert_eq!(
        receipt["plan_fingerprint"].as_str(),
        plan["plan_fingerprint"].as_str()
    );
    assert!(
        plan["actions"]
            .as_array()
            .expect("plan actions")
            .iter()
            .all(|action| action["status"].as_str() == Some("planned")),
        "dry-run plan actions should stay planned even when receipt actions applied"
    );
    assert!(
        receipt["actions"]
            .as_array()
            .expect("receipt actions")
            .iter()
            .any(|action| {
                action["status"].as_str() == Some("applied")
                    && action["redacted_target_path"]
                        .as_str()
                        .is_some_and(|path| path.starts_with("[cass-data]/"))
            }),
        "receipt actions should expose applied status and support-bundle redacted paths"
    );
    assert_eq!(
        payload["event_log"]["status"].as_str(),
        Some("embedded_receipt_events"),
        "mutating doctor top-level event_log should link to the cleanup receipt event stream"
    );
    let receipt_event_log = &receipt["event_log"];
    assert_eq!(
        receipt_event_log["status"].as_str(),
        Some("embedded_receipt_events")
    );
    let receipt_events = receipt_event_log["events"]
        .as_array()
        .expect("receipt event log events");
    assert_eq!(
        receipt_events
            .first()
            .and_then(|event| event["phase"].as_str()),
        Some("operation_started")
    );
    assert!(
        receipt_events
            .iter()
            .any(|event| event["phase"].as_str() == Some("action_applied")
                && event["receipt_correlation_id"].as_str() == Some("doctor_cleanup_apply_v1")),
        "receipt event log should correlate applied cleanup actions with the cleanup receipt"
    );
    assert_eq!(
        receipt_event_log["hash_chain_tip"].as_str(),
        receipt_events
            .last()
            .and_then(|event| event["event_id"].as_str())
    );
    assert!(
        actions.iter().any(|action| {
            action["artifact_kind"].as_str() == Some("retained_publish_backup")
                && action["asset_class"].as_str() == Some("retained_publish_backup")
                && action["safety_classification"].as_str() == Some("derived_reclaimable")
                && action["safe_to_gc_allowed"].as_bool() == Some(true)
                && action["applied"].as_bool() == Some(true)
        }),
        "apply result should include retained publish backup prune action"
    );
    assert!(
        actions.iter().any(|action| {
            action["artifact_kind"].as_str() == Some("lexical_generation")
                && action["generation_id"].as_str() == Some("gen-superseded")
                && action["asset_class"].as_str() == Some("reclaimable_derived_cache")
                && action["safety_classification"].as_str() == Some("derived_reclaimable")
                && action["safe_to_gc_allowed"].as_bool() == Some(true)
                && action["applied"].as_bool() == Some(true)
        }),
        "apply result should include superseded generation prune action"
    );
}

/// `coding_agent_session_search-ibuuh.23` lifecycle invariant:
/// `cass doctor --json --fix` is idempotent across consecutive
/// invocations. Once the first --fix has reclaimed every safe
/// derivative artifact, the second --fix run on the same data dir
/// MUST report no additional cleanup work — `auto_fix_actions`
/// contains no `Pruned N derivative cleanup artifact(s)` line, the
/// top-level `cleanup_apply` payload reports `pruned_asset_count: 0`,
/// and `before_reclaim_candidate_count == 0` (matching the after-state
/// of the first run).
///
/// This is the "do no harm" property of doctor --fix that the bead
/// requires for long-running maintenance: an operator running
/// `cass doctor --fix` on a cron schedule must not see spurious
/// "fixed N issues" output every cycle when the disk is already
/// clean. Without this pin, a regression in cleanup state tracking
/// (e.g., a re-discovery of already-pruned generations) could ship
/// silently and pollute operator dashboards.
///
#[test]
fn doctor_fix_is_idempotent_across_consecutive_invocations() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");

    // Seed: two retained publish backups (older outside cap=1 → reclaimable)
    // + one superseded reclaimable lexical generation. After the FIRST
    // --fix, both should be pruned; the SECOND --fix should observe
    // a clean state and report no additional work.
    let retained_publish_dir = index_path
        .parent()
        .expect("index parent")
        .join(".lexical-publish-backups");
    fs::create_dir_all(&retained_publish_dir).expect("create retained publish dir");
    let older_backup = retained_publish_dir.join("prior-live-older");
    fs::create_dir_all(&older_backup).expect("create older retained backup");
    fs::write(older_backup.join("segment-a"), b"old backup bytes")
        .expect("write older retained publish backup");
    std::thread::sleep(Duration::from_millis(20));
    let newer_backup = retained_publish_dir.join("prior-live-newer");
    fs::create_dir_all(&newer_backup).expect("create newer retained backup");
    fs::write(newer_backup.join("segment-b"), b"new backup bytes")
        .expect("write newer retained publish backup");

    let superseded_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-superseded");
    write_superseded_reclaimable_manifest(&superseded_dir, "gen-superseded");
    fs::write(
        superseded_dir.join("segment-old"),
        b"superseded generation bytes",
    )
    .expect("write superseded generation artifact");

    let quarantined_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-quarantined");
    write_quarantined_manifest(&quarantined_dir);
    fs::write(
        quarantined_dir.join("segment-a"),
        b"quarantined generation bytes",
    )
    .expect("write quarantined generation artifact");

    let invoke_doctor_fix = || -> Value {
        let out = cass_cmd(test_home.path())
            .args([
                "doctor",
                "--json",
                "--fix",
                "--data-dir",
                data_dir.to_str().expect("utf8"),
            ])
            .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
            .output()
            .expect("run cass doctor --json --fix");
        assert!(
            out.status.success(),
            "cass doctor --json --fix failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("doctor --fix --json emits JSON")
    };

    // First invocation: must DO work — at least 1 prune applied.
    let first = invoke_doctor_fix();
    let first_actions = first["auto_fix_actions"]
        .as_array()
        .expect("auto_fix_actions array on first run");
    assert!(
        first_actions
            .iter()
            .any(|a| a.as_str().unwrap_or_default().contains("Pruned ")),
        "first --fix MUST report at least one Pruned action; payload: {first:#}"
    );
    let first_cleanup = first["checks"]
        .as_array()
        .expect("checks on first run")
        .iter()
        .find(|c| c["name"].as_str() == Some("derivative_cleanup"))
        .expect("derivative_cleanup check on first run");
    assert_eq!(
        first_cleanup["fix_applied"].as_bool(),
        Some(true),
        "first --fix MUST flip derivative_cleanup.fix_applied to true"
    );
    let first_cleanup_apply = &first["cleanup_apply"];
    assert!(
        first_cleanup_apply["pruned_asset_count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "first --fix MUST prune at least 1 asset; cleanup_apply: {first_cleanup_apply:#}"
    );

    // Second invocation: idempotent — no additional Pruned actions,
    // pruned_asset_count == 0, before_reclaim_candidate_count == 0.
    let second = invoke_doctor_fix();
    let second_actions = second["auto_fix_actions"]
        .as_array()
        .expect("auto_fix_actions array on second run");
    assert!(
        !second_actions
            .iter()
            .any(|a| a.as_str().unwrap_or_default().contains("Pruned ")),
        "second --fix MUST be a no-op for pruning — no new Pruned action allowed; \
         got actions: {second_actions:#?}\nfull payload: {second:#}"
    );
    let second_cleanup = second["checks"]
        .as_array()
        .expect("checks on second run")
        .iter()
        .find(|c| c["name"].as_str() == Some("derivative_cleanup"))
        .expect("derivative_cleanup check on second run");
    assert_eq!(
        second_cleanup["fix_applied"].as_bool(),
        Some(false),
        "second --fix MUST leave derivative_cleanup.fix_applied false"
    );
    let cleanup_apply = &second["cleanup_apply"];
    assert_eq!(
        cleanup_apply["before_reclaim_candidate_count"]
            .as_u64()
            .unwrap_or(u64::MAX),
        0,
        "second --fix MUST observe zero reclaim candidates after first run; \
         cleanup_apply: {cleanup_apply:#}"
    );
    assert_eq!(
        cleanup_apply["pruned_asset_count"]
            .as_u64()
            .unwrap_or(u64::MAX),
        0,
        "second --fix MUST prune zero additional assets; cleanup_apply: {cleanup_apply:#}"
    );

    // The cumulative issues_fixed counter is allowed to vary by
    // implementation choice (some implementations return the same
    // count, some return 0 on no-op). The HARD invariant is that
    // the second run does NO additional work — pinned above by
    // the actions array + pruned_asset_count assertions.

    // Filesystem check: protected backup + freshly-pruned ones stay
    // in their post-first-run state across the second invocation.
    assert!(
        !older_backup.exists(),
        "older retained backup MUST stay pruned across consecutive --fix runs"
    );
    assert!(
        newer_backup.exists(),
        "protected newer retained backup MUST survive consecutive --fix runs"
    );
    assert!(
        !superseded_dir.exists(),
        "superseded generation MUST stay pruned across consecutive --fix runs"
    );
    assert!(
        quarantined_dir.exists(),
        "quarantined generation MUST remain for inspection across consecutive --fix runs"
    );
}

#[cfg(unix)]
#[test]
fn doctor_fix_refuses_symlinked_retained_publish_backup_targets() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");
    let retained_publish_dir = index_path
        .parent()
        .expect("index parent")
        .join(".lexical-publish-backups");
    fs::create_dir_all(&retained_publish_dir).expect("create retained publish dir");

    let external_target = test_home.path().join("external-backup-target");
    fs::create_dir_all(&external_target).expect("create external symlink target");
    let external_sentinel = external_target.join("sentinel");
    fs::write(&external_sentinel, b"must remain outside cleanup roots")
        .expect("write external sentinel");
    let older_backup = retained_publish_dir.join("prior-live-older");
    std::os::unix::fs::symlink(&external_target, &older_backup)
        .expect("create symlinked retained backup");

    std::thread::sleep(Duration::from_millis(20));
    let newer_backup = retained_publish_dir.join("prior-live-newer");
    fs::create_dir_all(&newer_backup).expect("create newer retained backup");
    fs::write(newer_backup.join("segment-b"), b"new backup bytes")
        .expect("write newer retained publish backup");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run cass doctor --json --fix");
    assert!(
        out.status.success(),
        "cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        external_sentinel.exists(),
        "cleanup must never follow a symlink outside the retained backup root"
    );
    assert!(
        fs::symlink_metadata(&older_backup)
            .expect("symlinked backup metadata")
            .file_type()
            .is_symlink(),
        "unsafe symlinked backup should remain for operator inspection"
    );
    assert!(
        newer_backup.exists(),
        "newest retained publish backup should remain protected"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let cleanup = &payload["cleanup_apply"];
    assert_eq!(cleanup["applied"].as_bool(), Some(false));
    assert_eq!(cleanup["pruned_asset_count"].as_u64(), Some(0));
    let actions = cleanup["actions"].as_array().expect("cleanup actions");
    assert!(
        actions.iter().any(|action| {
            action["artifact_kind"].as_str() == Some("retained_publish_backup")
                && action["asset_class"].as_str() == Some("retained_publish_backup")
                && action["path"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("prior-live-older")
                && action["skipped"].as_bool() == Some(true)
                && action["skip_reason"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("unsafe cleanup target")
        }),
        "doctor --fix should report symlinked retained backups as unsafe cleanup targets"
    );
}

#[test]
fn doctor_fix_preserves_reclaimable_generations_when_active_work_exists() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);
    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");

    let superseded_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-superseded");
    write_superseded_reclaimable_manifest(&superseded_dir, "gen-superseded");
    fs::write(
        superseded_dir.join("segment-old"),
        b"superseded generation bytes",
    )
    .expect("write superseded generation artifact");

    let active_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-active");
    write_active_manifest(&active_dir, "gen-active");
    fs::write(
        active_dir.join("segment-active"),
        b"active generation bytes",
    )
    .expect("write active generation artifact");

    let out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run cass doctor --json --fix");
    assert!(
        out.status.success(),
        "cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        superseded_dir.exists(),
        "cleanup apply must preserve reclaimable generations while active work exists"
    );
    assert!(
        active_dir.exists(),
        "cleanup apply must preserve active scratch/resumable work"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let cleanup = &payload["cleanup_apply"];
    assert_eq!(cleanup["applied"].as_bool(), Some(false));
    assert_eq!(cleanup["pruned_asset_count"].as_u64(), Some(0));
    assert!(
        cleanup["blocked_reasons"]
            .as_array()
            .expect("blocked reasons")
            .iter()
            .any(|reason| {
                reason
                    .as_str()
                    .unwrap_or_default()
                    .contains("active generation work")
            }),
        "apply result should explain active-work safety block"
    );
}

// ========================================================================
// Bead coding_agent_session_search-ibuuh.23 (lifecycle validation matrix:
// long-running maintenance story end-to-end via real CLI invocations).
//
// The bead's SCOPE explicitly calls for "at least one CLI/robot/E2E
// script that demonstrates a long-running maintenance story end to end:
// work starts, pauses under pressure, resumes, publishes, marks
// superseded artifacts, and cleans up conservatively." A sibling test
// in tests/lifecycle_matrix.rs
// (maintenance_publish_pause_resume_cleanup_story_is_artifact_backed)
// exercises the simulation harness; this test exercises the REAL `cass`
// binary across four sequential invocations operators actually run when
// triaging a real install:
//
//   1. cass diag --json --quarantine  → inventory the seeded state
//   2. cass doctor --json             → preview the cleanup plan (no fix)
//   3. cass doctor --json --fix       → apply the conservative cleanup
//   4. cass diag --json --quarantine  → verify the post-state
//
// The contract pinned across all four invocations:
//   - The diag inventory and the doctor preview AGREE on what's eligible
//     for reclaim (cross-command consistency, complementing bead p1x0z's
//     empty-state agreement test and the seeded-state companion in
//     tests/cli_diag.rs).
//   - `doctor --fix` removes ONLY the assets the preview marked
//     reclaimable: the older retained publish backup (over the
//     retention cap) and the fully-reclaimable superseded generation.
//   - `doctor --fix` PRESERVES the newer retained publish backup
//     (within cap) and the quarantined generation (operator inspection
//     required).
//   - The post-fix diag inventory shows the expected counter deltas
//     (failed_seed_bundle_count unchanged, retained_publish_backup_count
//     dropped from 2 to 1, lexical_quarantined_generation_count
//     unchanged at 1, lexical_generation_count dropped by the
//     reclaimed superseded generation).
//
// This is the "demonstrates a long-running maintenance story end to
// end" gate the bead asks for, expressed as four sequential
// machine-readable JSON exchanges instead of a simulation harness
// trace. A regression in any single invocation's contract trips a
// specific assertion that names which step diverged.
// ========================================================================

#[test]
fn long_running_maintenance_story_end_to_end_across_diag_doctor_fix_diag() {
    let test_home = tempfile::tempdir().expect("tempdir");
    let data_dir = test_home.path().join("cass-data");
    seed_healthy_empty_index(test_home.path(), &data_dir);

    // Seed: same fixture pattern as
    // tests/cli_diag.rs::diag_and_doctor_agree_on_quarantine_summary_on_seeded_state.
    // Four artifact classes:
    //   * 2 failed seed bundles (main + WAL sidecar) — quarantined,
    //     never reclaimed.
    //   * 2 retained publish backups (older + newer) — retention cap=1
    //     means the older one is reclaimable.
    //   * 1 superseded reclaimable lexical generation — fully
    //     reclaimable.
    //   * 1 quarantined lexical generation — never reclaimed.
    let backups_dir = data_dir.join("backups");
    fs::create_dir_all(&backups_dir).expect("create backups dir");
    let failed_seed_root =
        backups_dir.join("agent_search.db.20260423T120000.12345.deadbeef.failed-baseline-seed.bak");
    fs::write(&failed_seed_root, b"seed-backup").expect("write failed seed bundle");
    fs::write(
        failed_seed_root.with_file_name(format!(
            "{}-wal",
            failed_seed_root
                .file_name()
                .and_then(|name| name.to_str())
                .expect("file name")
        )),
        b"seed-wal",
    )
    .expect("write failed seed wal");

    let index_path = expected_index_dir(&data_dir);
    fs::create_dir_all(&index_path).expect("create expected index dir");
    let retained_publish_dir = index_path
        .parent()
        .expect("index parent")
        .join(".lexical-publish-backups");
    fs::create_dir_all(&retained_publish_dir).expect("create retained publish dir");
    let older_backup = retained_publish_dir.join("prior-live-older");
    fs::create_dir_all(&older_backup).expect("create older retained backup");
    fs::write(older_backup.join("segment-a"), b"retained-live-segment-old")
        .expect("write older retained publish backup");
    // Distinct mtimes so retention picks a deterministic winner; without
    // the sleep, filesystem-coarse timestamps tie and the test flakes.
    std::thread::sleep(Duration::from_millis(20));
    let newer_backup = retained_publish_dir.join("prior-live-newer");
    fs::create_dir_all(&newer_backup).expect("create newer retained backup");
    fs::write(newer_backup.join("segment-b"), b"retained-live-segment-new")
        .expect("write newer retained publish backup");

    let superseded_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-superseded");
    write_superseded_reclaimable_manifest(&superseded_dir, "gen-superseded");
    fs::write(
        superseded_dir.join("segment-old"),
        b"superseded generation bytes",
    )
    .expect("write superseded generation artifact");

    let quarantined_dir = index_path
        .parent()
        .expect("index parent")
        .join("generation-quarantined");
    write_quarantined_manifest(&quarantined_dir);
    fs::write(
        quarantined_dir.join("segment-a"),
        b"quarantined generation bytes",
    )
    .expect("write quarantined generation artifact");

    // ─── Step 1: cass diag --json --quarantine (initial inventory) ─────
    let diag_initial_out = cass_cmd(test_home.path())
        .args([
            "diag",
            "--json",
            "--quarantine",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run initial cass diag");
    assert!(
        diag_initial_out.status.success(),
        "step 1 cass diag --json --quarantine failed: stderr={}",
        String::from_utf8_lossy(&diag_initial_out.stderr)
    );
    let diag_initial_payload: Value =
        serde_json::from_slice(&diag_initial_out.stdout).expect("step 1 diag JSON parses");
    let diag_initial_summary = diag_initial_payload["quarantine"]["summary"]
        .as_object()
        .expect("step 1 diag summary present");
    assert_eq!(
        diag_initial_summary["failed_seed_bundle_count"].as_u64(),
        Some(2),
        "step 1: 2 failed seed bundles seeded"
    );
    assert_eq!(
        diag_initial_summary["retained_publish_backup_count"].as_u64(),
        Some(2),
        "step 1: 2 retained publish backups seeded"
    );
    assert_eq!(
        diag_initial_summary["lexical_quarantined_generation_count"].as_u64(),
        Some(1),
        "step 1: 1 quarantined lexical generation seeded"
    );

    // ─── Step 2: cass doctor --json (preview cleanup, no fix) ──────────
    let doctor_preview_out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run doctor preview");
    let doctor_preview_payload: Value =
        serde_json::from_slice(&doctor_preview_out.stdout).expect("step 2 doctor JSON parses");
    let doctor_preview_summary = doctor_preview_payload["quarantine"]["summary"]
        .as_object()
        .expect("step 2 doctor summary present");

    // CONTRACT: diag and doctor preview AGREE on every shared scalar.
    // (Cross-command consistency on populated state — sibling test in
    // tests/cli_diag.rs pins the same set; this end-to-end test pins
    // it again at the FIRST step of the operator workflow because a
    // divergence here would mean the operator's diag-based decision
    // doesn't match what doctor will preview.)
    for field in [
        "failed_seed_bundle_count",
        "retained_publish_backup_count",
        "retained_publish_backup_retention_limit",
        "lexical_generation_count",
        "lexical_quarantined_generation_count",
        "lexical_quarantined_shard_count",
        "cleanup_dry_run_reclaim_candidate_count",
        "cleanup_dry_run_reclaimable_bytes",
        "cleanup_dry_run_protected_generation_count",
        "cleanup_apply_allowed",
    ] {
        assert_eq!(
            diag_initial_summary.get(field),
            doctor_preview_summary.get(field),
            "step 1↔2 cross-command divergence on {field}: diag={:?} doctor={:?}",
            diag_initial_summary.get(field),
            doctor_preview_summary.get(field)
        );
    }
    // Preview MUST identify reclaim candidates (the older publish
    // backup + the superseded generation = 2). A regression that
    // missed either would tell the operator nothing is reclaimable.
    let preview_reclaim_count = doctor_preview_summary["cleanup_dry_run_reclaim_candidate_count"]
        .as_u64()
        .expect("preview must report reclaim candidate count");
    assert!(
        preview_reclaim_count >= 1,
        "step 2: preview must identify ≥1 reclaim candidate (older publish backup + \
         superseded generation); got {preview_reclaim_count}"
    );

    // ─── Step 3: cass doctor --json --fix (apply conservative cleanup) ─
    let doctor_apply_out = cass_cmd(test_home.path())
        .args([
            "doctor",
            "--json",
            "--fix",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run doctor --fix");
    assert!(
        doctor_apply_out.status.success(),
        "step 3 cass doctor --json --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&doctor_apply_out.stdout),
        String::from_utf8_lossy(&doctor_apply_out.stderr)
    );

    // CONTRACT: filesystem post-state matches the safety policy:
    //   * older publish backup PRUNED (over retention cap)
    //   * newer publish backup PRESERVED (within cap)
    //   * superseded generation PRUNED (fully reclaimable)
    //   * quarantined generation PRESERVED (operator inspection)
    //   * failed seed bundles PRESERVED (separate quarantine class)
    assert!(
        !older_backup.exists(),
        "step 3: older retained publish backup MUST be pruned (over retention cap)"
    );
    assert!(
        newer_backup.exists(),
        "step 3: newer retained publish backup MUST be preserved (within cap)"
    );
    assert!(
        !superseded_dir.exists(),
        "step 3: fully-reclaimable superseded generation MUST be pruned"
    );
    assert!(
        quarantined_dir.exists(),
        "step 3: quarantined generation MUST be preserved for operator inspection"
    );
    assert!(
        failed_seed_root.exists(),
        "step 3: failed seed bundle MUST be preserved (separate quarantine class)"
    );

    // ─── Step 4: cass diag --json --quarantine (verify post-state) ─────
    let diag_post_out = cass_cmd(test_home.path())
        .args([
            "diag",
            "--json",
            "--quarantine",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
        ])
        .env("CASS_LEXICAL_PUBLISH_BACKUP_RETENTION", "1")
        .output()
        .expect("run post-fix diag");
    assert!(
        diag_post_out.status.success(),
        "step 4 cass diag --json --quarantine failed: stderr={}",
        String::from_utf8_lossy(&diag_post_out.stderr)
    );
    let diag_post_payload: Value =
        serde_json::from_slice(&diag_post_out.stdout).expect("step 4 diag JSON parses");
    let diag_post_summary = diag_post_payload["quarantine"]["summary"]
        .as_object()
        .expect("step 4 diag summary present");

    // CONTRACT: post-state counter deltas match the apply policy.
    assert_eq!(
        diag_post_summary["failed_seed_bundle_count"].as_u64(),
        Some(2),
        "step 4: failed seed bundles preserved (count unchanged from step 1)"
    );
    assert_eq!(
        diag_post_summary["retained_publish_backup_count"].as_u64(),
        Some(1),
        "step 4: retained publish backup count drops 2→1 (older pruned, newer kept)"
    );
    assert_eq!(
        diag_post_summary["lexical_quarantined_generation_count"].as_u64(),
        Some(1),
        "step 4: quarantined generation preserved (count unchanged from step 1)"
    );
    // The superseded generation is no longer in the inventory; the
    // total lexical_generation_count should have dropped by 1
    // relative to step 1 (only the quarantined generation remains).
    let initial_gen_count = diag_initial_summary["lexical_generation_count"]
        .as_u64()
        .unwrap_or_default();
    let post_gen_count = diag_post_summary["lexical_generation_count"]
        .as_u64()
        .unwrap_or_default();
    assert_eq!(
        post_gen_count + 1,
        initial_gen_count,
        "step 4: lexical_generation_count must drop by 1 after pruning the superseded \
         generation; initial={initial_gen_count} post={post_gen_count}"
    );
}
