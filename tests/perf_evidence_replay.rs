use coding_agent_search::perf_evidence::{
    PerfArtifactRef, PerfCount, PerfCountPrecision, PerfEvidenceLedger, PerfMachineProfile,
    PerfPhaseKind, PerfPhaseTiming, PerfProofStatus, PerfProofSummary, PerfReplayGate,
    PerfReplayMetric, PerfReplayThresholds, PerfReplayVerdict, PerfSearchSnapshot, PerfWorkload,
    PerfWorkloadKind, read_perf_evidence_ledger, write_perf_evidence_ledger,
};

fn ledger(run_id: &str, p99_ms: u64, elapsed_ms: u64) -> PerfEvidenceLedger {
    let mut ledger = PerfEvidenceLedger::new(
        run_id,
        PerfWorkload {
            kind: PerfWorkloadKind::Search,
            name: "saved-artifact-search".to_string(),
            description: Some("integration fixture for saved perf evidence replay".to_string()),
            command_args: vec![
                "cass".to_string(),
                "search".to_string(),
                "memory pressure".to_string(),
                "--json".to_string(),
            ],
            input_count: Some(PerfCount {
                value: 10_000,
                precision: PerfCountPrecision::LowerBound,
            }),
        },
        1_780_000_000_000,
    );
    ledger.machine = PerfMachineProfile {
        logical_cpus: Some(64),
        reserved_cores: Some(8),
        available_memory_bytes: Some(256 * 1024 * 1024 * 1024),
        topology_class: Some("single_host_many_core".to_string()),
    };
    ledger.search = Some(PerfSearchSnapshot {
        query_hash: "blake3:integration-fixture".to_string(),
        limit: 20,
        matched_count: Some(PerfCount {
            value: 250,
            precision: PerfCountPrecision::Exact,
        }),
        returned_hits: 20,
        requested_mode: "hybrid".to_string(),
        realized_mode: "hybrid".to_string(),
        fallback_tier: None,
        timed_out: false,
    });
    ledger.phases = vec![
        phase("queue", PerfPhaseKind::Queueing, elapsed_ms / 4, p99_ms / 4),
        phase(
            "service",
            PerfPhaseKind::Service,
            elapsed_ms / 4,
            p99_ms / 4,
        ),
        phase(
            "hydrate",
            PerfPhaseKind::Hydration,
            elapsed_ms / 4,
            p99_ms / 4,
        ),
        phase("output", PerfPhaseKind::Output, elapsed_ms / 4, p99_ms / 4),
    ];
    ledger.proof = PerfProofSummary {
        status: PerfProofStatus::Passed,
        baseline_artifact: None,
        comparison_artifact: None,
        p99_regression_basis_points: None,
        notes: vec!["integration fixture proof".to_string()],
    };
    ledger.artifacts = vec![PerfArtifactRef {
        label: "fixture-source".to_string(),
        path: "tests/perf_evidence_replay.rs".to_string(),
        kind: "rust-test".to_string(),
        sha256: None,
    }];
    ledger
}

fn phase(name: &str, kind: PerfPhaseKind, elapsed_ms: u64, p99_ms: u64) -> PerfPhaseTiming {
    PerfPhaseTiming {
        name: name.to_string(),
        kind,
        elapsed_ms,
        p50_ms: Some(p99_ms.saturating_sub(3)),
        p95_ms: Some(p99_ms.saturating_sub(1)),
        p99_ms: Some(p99_ms),
        samples: Some(PerfCount {
            value: 40,
            precision: PerfCountPrecision::Exact,
        }),
    }
}

#[test]
fn replay_harness_writes_reads_and_gates_saved_ledger_artifacts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let baseline_path = tmp.path().join("baseline.json");
    let current_path = tmp.path().join("current.json");
    let baseline = ledger("baseline-run", 40, 80);
    let current = ledger("current-run", 64, 128);

    let baseline_artifact =
        write_perf_evidence_ledger(&baseline, &baseline_path).expect("write baseline");
    let current_artifact =
        write_perf_evidence_ledger(&current, &current_path).expect("write current");

    assert_eq!(baseline_artifact.kind, "json");
    assert!(baseline_artifact.sha256.is_some());
    assert_eq!(current_artifact.kind, "json");
    assert!(current_artifact.sha256.is_some());

    let decoded = read_perf_evidence_ledger(&current_path).expect("read current");
    assert_eq!(decoded.run_id, "current-run");
    assert_eq!(decoded.workload.command_args[0], "cass");

    let gate = PerfReplayGate::new(
        PerfReplayThresholds::try_new(500, 1_000, 500, 1_000).expect("thresholds"),
    );
    let report = gate
        .replay_files(&current_path, Some(&baseline_path))
        .expect("replay saved artifacts");

    assert_eq!(report.verdict, PerfReplayVerdict::Failure);
    assert!(report.should_fail_build());
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.metric == PerfReplayMetric::ComposedP99),
        "{report:#?}"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.metric == PerfReplayMetric::TotalElapsed),
        "{report:#?}"
    );
    assert!(
        report.logs.iter().any(|event| {
            event.artifact_path.as_deref() == Some(current_path.to_str().unwrap())
                && event.run_id == "current-run"
                && event.command_args == ["cass", "search", "memory pressure", "--json"]
                && event.failure_reason.is_some()
        }),
        "{report:#?}"
    );
}
