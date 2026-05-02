# CASS lexical rebuild page-prep fanout perf slice

Date: 2026-05-02

## Workload

- Command shape: `cass index --watch-once <nonexistent> --data-dir <db-only-copy> --json --progress-interval-ms 5000`
- Seed database: `/home/ubuntu/cass-post-tokenizer-hotspot-20260502T035907Z/agent_search.db*`
- Seed copy mode: DB files only; derived lexical index omitted so the authoritative rebuild path runs.
- Corpus: 51,214 conversations / 4,711,459 canonical messages
- Binary: `/data/tmp/cass_perf_opt_target/profiling/cass`
- Changed default: `CASS_TANTIVY_REBUILD_PAGE_PREP_WORKERS` fallback cap `16 -> 8`

## Baseline

Artifact: `tests/artifacts/perf/cass-pageprep-default-rebuild-head-20260502T115045Z`

- `elapsed_ms`: 48,935
- Wall time: 0:49.74
- Full corpus reached: 40,929 ms
- Phase returned to preparing: 44,632 ms
- Max RSS: 60,873,176 KB
- Producer handoff wait: 920 waits / 5,093 ms
- Page-prep workers: 16

## Candidate

Artifact: `tests/artifacts/perf/cass-pageprep8-default-code-20260502T115045Z`

- `elapsed_ms`: 45,732
- Wall time: 0:46.94
- Full corpus reached: 39,828 ms
- Phase returned to preparing: 41,829 ms
- Max RSS: 46,182,688 KB
- Producer handoff wait: 426 waits / 2,893 ms
- Page-prep workers: 8

## Delta

- Total `elapsed_ms`: 6.5% faster (`48,935 -> 45,732`)
- Wall time: 5.6% faster (`49.74s -> 46.94s`)
- Max RSS: 24.1% lower (`60,873,176 KB -> 46,182,688 KB`)
- Producer handoff wait: 53.7% fewer waits and 43.2% fewer wait ms
- Full-corpus handoff: 2.7% faster (`40,929 ms -> 39,828 ms`)

## A/B notes

- A pre-code env override run with `CASS_TANTIVY_REBUILD_PAGE_PREP_WORKERS=8` produced the same behavioral shape: `elapsed_ms=47,534`, wall `48.94s`, max RSS `46,328,820 KB`, and 403 handoff waits / 3,048 ms.
- Whole-seed copies are no longer valid for this rebuild workload because the seed now contains a valid lexical index; those no-op watch samples were retained only as rejected setup evidence.
- The env override remains available, so operators can still raise page-prep fanout above the new default when a different storage shape benefits from it.

## Interpretation

The previous 16-worker default overfed the DB read/prep side on the large lexical rebuild. More eager page-prep workers increased memory pressure and left the producer waiting longer at the ordered sink. Capping the default at 8 preserves producer overlap without letting prepared-page and shard-builder pressure dominate. The change does not alter ordering, document selection, shard boundaries, or the operator override path.

## Behavior proof

- Ordering preserved: yes. Page-prep workers still return sequenced pages, and ordered emission remains in the producer.
- Document set preserved: yes. Baseline and candidate both rebuilt 51,214 conversations / 4,711,459 canonical messages.
- Query smoke: `function` on baseline and candidate both returned `total_matches=241394` with identical top 5 hits.
- Fallback/operator control: yes. `CASS_TANTIVY_REBUILD_PAGE_PREP_WORKERS` still overrides the default.

## Verification

- `cargo test -q lexical_rebuild_default_page_prep_worker_parallelism_stays_bounded_without_channel_cap --lib`
- `cargo test -q lexical_rebuild_pipeline_settings_snapshot_defaults_page_prep_workers_from_worker_budget --lib`
- Search smoke against baseline and candidate data dirs for `function`
- `cargo fmt --check`
- `cargo check --all-targets`
- `cargo clippy --all-targets -- -D warnings`
