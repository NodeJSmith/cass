// Dead-code tolerated module-wide: this storage-integrity diagnostic
// contract lands ahead of the probes that populate it (.14.3 concurrency /
// busy-lock / WAL diagnostics) and the backup-first repair planner (.14.2),
// and the health/status/doctor/fleet/support-bundle surfaces that project it.
#![allow(dead_code)]

//! Storage-integrity diagnostic taxonomy and JSON contract (bead
//! cass-fleet-resilience-20260608-uojcg.14.1).
//!
//! Storage failures surface today as scattered symptoms — OpenRead cursor
//! errors, integrity-check failures, stale WAL/SHM sidecars, schema-version
//! drift, busy locks, FTS metadata mismatch, legacy-DB readability problems,
//! unsafe SQL construction, and zero-result regressions — with no shared
//! vocabulary. Without one, doctor/status give generic "stale index" advice
//! when the operator actually needs archive-risk handling.
//!
//! This module defines the single contract every storage surface (health,
//! status, doctor, triage, fleet, search metadata, support bundles) projects:
//! a [`StorageState`], a [`SourceOfTruthRisk`], an [`ArchiveReadability`],
//! and the [`StorageCheck`]s attempted (each carrying `elapsed_ms`,
//! `timed_out`, an optional `skipped_reason`, and whether it is read-only).
//! [`StorageIntegrityReport::derive`] computes the source-of-truth risk from
//! the state so robot JSON and human summaries agree.
//!
//! This is the schema/contract only. The probes that populate it must use
//! bound parameters for variable SQL values and add no new rusqlite code —
//! that is the `.14.2`/`.14.3` implementation. All enums serialize as
//! snake_case, matching the readiness vocabulary; the associated root-cause
//! family reuses [`crate::root_cause_taxonomy::RootCauseFamily`].

use serde::{Deserialize, Serialize};

use crate::root_cause_taxonomy::RootCauseFamily;

/// The storage-engine integrity state. `Ok` and the failure modes the report
/// enumerated; `UnknownDeferred` is the explicit fallback when a check could
/// not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StorageState {
    /// All attempted checks passed.
    Ok,
    /// Only derived assets drifted; the canonical DB itself is intact.
    DerivedOnlyDrift,
    /// The DB is busy or locked by another writer.
    BusyOrLocked,
    /// A WAL/SHM sidecar is suspect (stale, orphaned, or size-inconsistent).
    WalSidecarSuspect,
    /// The on-disk schema version drifted from the expected contract.
    SchemaDrift,
    /// A cursor/OpenRead operation failed.
    OpenreadFailed,
    /// An integrity / `PRAGMA integrity_check`-class check failed.
    IntegrityFailed,
    /// A legacy database could not be read by the current engine.
    LegacyInteropFailed,
    /// FTS metadata is missing or inconsistent.
    FtsMetadataFailed,
    /// An unsafe SQL construction / query shape (bind-risk) was detected.
    UnsafeSqlShape,
    /// A check could not run and the verdict is deferred — never omit it.
    UnknownDeferred,
}

/// Risk to the canonical source of truth implied by the storage state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceOfTruthRisk {
    None,
    Low,
    Medium,
    High,
    Unknown,
}

/// Whether the canonical archive could be read during the diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArchiveReadability {
    Readable,
    PartiallyReadable,
    Unreadable,
    NotChecked,
    TimedOut,
}

/// One diagnostic check that was attempted (or skipped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageCheck {
    /// Stable check name (snake_case), e.g. `open_read`, `integrity_check`.
    pub name: String,
    pub elapsed_ms: i64,
    pub timed_out: bool,
    /// Why the check was skipped, when it was. `None` when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Whether the check only reads (never mutates) the archive — true for
    /// every diagnostic probe; repairs are not checks.
    pub read_only: bool,
}

impl StorageCheck {
    /// A read-only check that ran to completion.
    pub(crate) fn ran(name: impl Into<String>, elapsed_ms: i64) -> Self {
        Self {
            name: name.into(),
            elapsed_ms,
            timed_out: false,
            skipped_reason: None,
            read_only: true,
        }
    }

    /// A read-only check that timed out.
    pub(crate) fn timed_out(name: impl Into<String>, elapsed_ms: i64) -> Self {
        Self {
            name: name.into(),
            elapsed_ms,
            timed_out: true,
            skipped_reason: None,
            read_only: true,
        }
    }

    /// A check that was skipped with a reason.
    pub(crate) fn skipped(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            elapsed_ms: 0,
            timed_out: false,
            skipped_reason: Some(reason.into()),
            read_only: true,
        }
    }
}

impl StorageState {
    /// The default source-of-truth risk implied by this state. Conservative:
    /// anything that prevents trusting/reading the canonical rows is high;
    /// derived-only / FTS issues are low because the canonical rows survive.
    pub(crate) fn default_risk(self) -> SourceOfTruthRisk {
        match self {
            Self::Ok => SourceOfTruthRisk::None,
            Self::FtsMetadataFailed | Self::DerivedOnlyDrift | Self::BusyOrLocked => {
                SourceOfTruthRisk::Low
            }
            Self::WalSidecarSuspect
            | Self::SchemaDrift
            | Self::LegacyInteropFailed
            | Self::UnsafeSqlShape => SourceOfTruthRisk::Medium,
            Self::OpenreadFailed | Self::IntegrityFailed => SourceOfTruthRisk::High,
            Self::UnknownDeferred => SourceOfTruthRisk::Unknown,
        }
    }

    /// The root-cause family this state attributes to. Storage states are
    /// frankensqlite-storage except the explicit deferred fallback.
    pub(crate) fn root_cause_family(self) -> RootCauseFamily {
        match self {
            Self::UnknownDeferred => RootCauseFamily::Unknown,
            _ => RootCauseFamily::FrankensqliteStorage,
        }
    }

    /// Whether ordinary search can still trust the canonical rows.
    pub(crate) fn canonical_trustworthy(self) -> bool {
        !matches!(self, Self::OpenreadFailed | Self::IntegrityFailed)
    }
}

/// The storage-integrity report every surface projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageIntegrityReport {
    pub storage_state: StorageState,
    pub source_of_truth_risk: SourceOfTruthRisk,
    pub archive_readability: ArchiveReadability,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks_attempted: Vec<StorageCheck>,
}

impl StorageIntegrityReport {
    /// Build a report, deriving `source_of_truth_risk` from the state so
    /// robot JSON and human summaries never disagree.
    pub(crate) fn derive(
        state: StorageState,
        archive_readability: ArchiveReadability,
        checks_attempted: Vec<StorageCheck>,
    ) -> Self {
        Self {
            storage_state: state,
            source_of_truth_risk: state.default_risk(),
            archive_readability,
            checks_attempted,
        }
    }

    /// A one-line human summary built from the SAME enum vocabulary the
    /// robot JSON serializes, so the two surfaces stay in lockstep.
    pub(crate) fn human_summary(&self) -> String {
        format!(
            "storage {} (source-of-truth risk {}, archive {})",
            serde_plain_label(self.storage_state),
            serde_plain_label(self.source_of_truth_risk),
            serde_plain_label(self.archive_readability),
        )
    }

    /// Whether every attempted check was read-only (a pure diagnostic pass
    /// never mutated the archive).
    pub(crate) fn all_checks_read_only(&self) -> bool {
        self.checks_attempted.iter().all(|c| c.read_only)
    }
}

/// Render an enum's snake_case wire label for human summaries (shared
/// vocabulary). Falls back to `unknown` if serialization is somehow not a
/// bare string (never expected for these unit enums).
fn serde_plain_label<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: &[StorageState] = &[
        StorageState::Ok,
        StorageState::DerivedOnlyDrift,
        StorageState::BusyOrLocked,
        StorageState::WalSidecarSuspect,
        StorageState::SchemaDrift,
        StorageState::OpenreadFailed,
        StorageState::IntegrityFailed,
        StorageState::LegacyInteropFailed,
        StorageState::FtsMetadataFailed,
        StorageState::UnsafeSqlShape,
        StorageState::UnknownDeferred,
    ];

    #[test]
    fn storage_state_values_serialize_snake_case_and_are_stable() {
        let pairs: &[(StorageState, &str)] = &[
            (StorageState::Ok, "ok"),
            (StorageState::DerivedOnlyDrift, "derived_only_drift"),
            (StorageState::BusyOrLocked, "busy_or_locked"),
            (StorageState::WalSidecarSuspect, "wal_sidecar_suspect"),
            (StorageState::SchemaDrift, "schema_drift"),
            (StorageState::OpenreadFailed, "openread_failed"),
            (StorageState::IntegrityFailed, "integrity_failed"),
            (StorageState::LegacyInteropFailed, "legacy_interop_failed"),
            (StorageState::FtsMetadataFailed, "fts_metadata_failed"),
            (StorageState::UnsafeSqlShape, "unsafe_sql_shape"),
            (StorageState::UnknownDeferred, "unknown_deferred"),
        ];
        for (v, want) in pairs {
            assert_eq!(serde_json::to_string(v).unwrap(), format!("\"{want}\""));
        }
        // Every variant is in the pinned list (count guard catches additions).
        assert_eq!(pairs.len(), ALL_STATES.len());
    }

    #[test]
    fn risk_and_readability_serialize_snake_case() {
        let risk: &[(SourceOfTruthRisk, &str)] = &[
            (SourceOfTruthRisk::None, "none"),
            (SourceOfTruthRisk::Low, "low"),
            (SourceOfTruthRisk::Medium, "medium"),
            (SourceOfTruthRisk::High, "high"),
            (SourceOfTruthRisk::Unknown, "unknown"),
        ];
        for (v, want) in risk {
            assert_eq!(serde_json::to_string(v).unwrap(), format!("\"{want}\""));
        }
        let read: &[(ArchiveReadability, &str)] = &[
            (ArchiveReadability::Readable, "readable"),
            (ArchiveReadability::PartiallyReadable, "partially_readable"),
            (ArchiveReadability::Unreadable, "unreadable"),
            (ArchiveReadability::NotChecked, "not_checked"),
            (ArchiveReadability::TimedOut, "timed_out"),
        ];
        for (v, want) in read {
            assert_eq!(serde_json::to_string(v).unwrap(), format!("\"{want}\""));
        }
    }

    #[test]
    fn every_state_has_a_defined_risk_and_storage_family() {
        for &s in ALL_STATES {
            // default_risk is total; Ok is the only None.
            let risk = s.default_risk();
            if s == StorageState::Ok {
                assert_eq!(risk, SourceOfTruthRisk::None);
            }
            // Every non-deferred state attributes to frankensqlite-storage.
            let fam = s.root_cause_family();
            if s == StorageState::UnknownDeferred {
                assert_eq!(fam, RootCauseFamily::Unknown);
            } else {
                assert_eq!(fam, RootCauseFamily::FrankensqliteStorage);
            }
        }
    }

    #[test]
    fn read_failures_are_high_risk_and_untrustworthy() {
        for s in [StorageState::OpenreadFailed, StorageState::IntegrityFailed] {
            assert_eq!(s.default_risk(), SourceOfTruthRisk::High, "{s:?}");
            assert!(!s.canonical_trustworthy(), "{s:?}");
        }
        // Derived-only / FTS / busy keep the canonical rows trustworthy.
        for s in [
            StorageState::DerivedOnlyDrift,
            StorageState::FtsMetadataFailed,
            StorageState::BusyOrLocked,
        ] {
            assert!(s.canonical_trustworthy(), "{s:?}");
            assert_eq!(s.default_risk(), SourceOfTruthRisk::Low, "{s:?}");
        }
    }

    /// Fixtures for the report's named failure modes.
    fn fixture(state: StorageState) -> StorageIntegrityReport {
        let (readability, checks) = match state {
            StorageState::OpenreadFailed => (
                ArchiveReadability::Unreadable,
                vec![StorageCheck::ran("open_read", 12)],
            ),
            StorageState::IntegrityFailed => (
                ArchiveReadability::PartiallyReadable,
                vec![StorageCheck::ran("integrity_check", 240)],
            ),
            StorageState::SchemaDrift => (
                ArchiveReadability::Readable,
                vec![StorageCheck::ran("schema_version", 3)],
            ),
            StorageState::BusyOrLocked => (
                ArchiveReadability::NotChecked,
                vec![StorageCheck::skipped(
                    "integrity_check",
                    "database is locked",
                )],
            ),
            StorageState::WalSidecarSuspect => (
                ArchiveReadability::Readable,
                vec![StorageCheck::ran("wal_sidecar", 5)],
            ),
            StorageState::UnsafeSqlShape => (
                ArchiveReadability::Readable,
                vec![StorageCheck::ran("sql_shape_lint", 1)],
            ),
            StorageState::FtsMetadataFailed => (
                ArchiveReadability::Readable,
                vec![StorageCheck::ran("fts_metadata", 8)],
            ),
            StorageState::LegacyInteropFailed => (
                ArchiveReadability::Unreadable,
                vec![StorageCheck::ran("legacy_open", 40)],
            ),
            _ => (
                ArchiveReadability::Readable,
                vec![StorageCheck::ran("open_read", 2)],
            ),
        };
        StorageIntegrityReport::derive(state, readability, checks)
    }

    #[test]
    fn fixtures_cover_the_named_failure_modes_with_consistent_risk() {
        let cases = [
            (StorageState::OpenreadFailed, SourceOfTruthRisk::High),
            (StorageState::IntegrityFailed, SourceOfTruthRisk::High),
            (StorageState::SchemaDrift, SourceOfTruthRisk::Medium),
            (StorageState::BusyOrLocked, SourceOfTruthRisk::Low),
            (StorageState::WalSidecarSuspect, SourceOfTruthRisk::Medium),
            (StorageState::UnsafeSqlShape, SourceOfTruthRisk::Medium),
            (StorageState::FtsMetadataFailed, SourceOfTruthRisk::Low),
            (StorageState::LegacyInteropFailed, SourceOfTruthRisk::Medium),
        ];
        for (state, risk) in cases {
            let r = fixture(state);
            assert_eq!(r.storage_state, state);
            assert_eq!(r.source_of_truth_risk, risk, "{state:?} risk");
            // Diagnostics never mutate the archive.
            assert!(
                r.all_checks_read_only(),
                "{state:?} checks must be read-only"
            );
        }
    }

    #[test]
    fn busy_lock_fixture_skips_with_a_reason() {
        let r = fixture(StorageState::BusyOrLocked);
        let check = &r.checks_attempted[0];
        assert!(check.skipped_reason.is_some());
        assert_eq!(r.archive_readability, ArchiveReadability::NotChecked);
    }

    #[test]
    fn human_summary_shares_the_robot_vocabulary() {
        let r = fixture(StorageState::OpenreadFailed);
        let summary = r.human_summary();
        // The human one-liner uses the exact serialized enum labels.
        assert!(summary.contains("openread_failed"), "{summary}");
        assert!(summary.contains("high"), "{summary}");
        assert!(summary.contains("unreadable"), "{summary}");
    }

    #[test]
    fn report_round_trips_through_json() {
        let r = fixture(StorageState::IntegrityFailed);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"storage_state\":\"integrity_failed\""));
        assert!(json.contains("\"source_of_truth_risk\":\"high\""));
        assert!(json.contains("\"archive_readability\":\"partially_readable\""));
        assert!(json.contains("\"read_only\":true"));
        let parsed: StorageIntegrityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn timed_out_check_is_recorded() {
        let r = StorageIntegrityReport::derive(
            StorageState::UnknownDeferred,
            ArchiveReadability::TimedOut,
            vec![StorageCheck::timed_out("integrity_check", 5000)],
        );
        assert_eq!(r.source_of_truth_risk, SourceOfTruthRisk::Unknown);
        assert!(r.checks_attempted[0].timed_out);
        assert_eq!(r.archive_readability, ArchiveReadability::TimedOut);
    }
}
