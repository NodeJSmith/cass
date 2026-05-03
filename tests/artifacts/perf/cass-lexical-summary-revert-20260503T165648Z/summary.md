# CASS lexical summary scan rollback measurement

Date: 2026-05-03

## Workload

- Binary: `/data/tmp/cass-target-summary-footprints-20260503/profiling/cass`
- Source DB seed: `/home/ubuntu/cass-large-rebuild-nextpass-footprints-valid-20260503T052123Z/agent_search.db`
- Data dir: `/home/ubuntu/cass-lexical-summary-revert-20260503T165648Z`
- Command:
  `timeout 140s env CASS_RESPONSIVENESS_DISABLE=1 CASS_PREP_PROFILE=1 .../cass index --watch-once ... --json --progress-interval-ms 5000 --color=never`

## Result

- Exit status: 0
- `/usr/bin/time` wall: 1:56.69
- CLI `elapsed_ms`: 115116
- Max RSS: 54747148 KB
- `CASS_PREP_PROFILE step=plan_lexical_shards`: 42825 ms
- Indexed conversations: 51214
- Indexed messages: 4711566

## Interpretation

The conversation-summary planner path was rejected:

- Unpaged summary scan artifact: `tests/artifacts/perf/cass-lexical-summary-footprints-20260503T161132Z`
- Paged summary scan artifact: `tests/artifacts/perf/cass-lexical-summary-footprints-paged-20260503T163506Z`
- Both timed out at 140s without reaching `plan_lexical_shards`.

This rollback restores the prior measured-safe grouped-message footprint path. It is still a hotspot, but it completes the full authoritative lexical rebuild under the 140s guard while the summary scans do not.
