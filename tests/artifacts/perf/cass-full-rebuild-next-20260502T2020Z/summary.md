# Full Lexical Rebuild Page-Prep Worker Tuning

Date: 2026-05-02
Host: `threadripperje`
Workload: repair a missing Tantivy lexical index from an existing 22.4 GB `agent_search.db`
Binary baseline commit: `a7072fff3b880b885c06cbb4ff680dd5e1d35dee`

## Workload

Each run used a reflinked copy of:

`/home/ubuntu/cass-post-tokenizer-hotspot-20260502T035907Z/agent_search.db`

Command shape:

```bash
env CASS_RESPONSIVENESS_DISABLE=1 CASS_PREP_PROFILE=1 \
  cass index \
  --watch-once /home/ubuntu/cass-full-rebuild-next-missing-<label>-20260502T2020Z.jsonl \
  --data-dir /home/ubuntu/cass-full-rebuild-next-<label>-20260502T2020Z \
  --json --progress-interval-ms 5000
```

The `pageprep4` and `pageprep6` probes additionally set
`CASS_TANTIVY_REBUILD_PAGE_PREP_WORKERS=4` or `6`.

## Results

| Label | Code/config | JSON elapsed ms | Wall | Max RSS KB | Conversations | Messages |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `default` | old default, 8 page-prep workers | 47,533 | 0:48.39 | 46,189,760 | 51,214 | 4,711,686 |
| `pageprep4` | env override, 4 page-prep workers | 49,837 | 0:50.49 | 33,871,912 | 51,214 | 4,711,686 |
| `pageprep6` | env override, 6 page-prep workers | 47,632 | 0:48.50 | 39,917,344 | 51,214 | 4,711,686 |
| `final` | new default, 6 page-prep workers | 48,542 | 0:49.86 | 40,141,048 | 51,214 | 4,711,686 |
| `final2` | new default, 6 page-prep workers | 45,533 | 0:46.43 | 40,340,816 | 51,214 | 4,711,686 |

Final-code average:

- Wall: 48.145s vs 48.39s baseline, effectively flat.
- JSON elapsed: 47,037.5 ms vs 47,533 ms baseline, about 1.0% lower.
- Max RSS: 40,240,932 KB vs 46,189,760 KB baseline, about 12.9% lower.

`pageprep4` saved more memory but slowed the rebuild, so it was rejected as the
default. Six page-prep workers kept the rebuild wall time in the noise band while
cutting peak RSS materially.

## Change

`lexical_rebuild_default_page_prep_worker_parallelism_for_workers()` now caps the
derived default at 6 instead of 8. The explicit
`CASS_TANTIVY_REBUILD_PAGE_PREP_WORKERS` override remains unchanged for operators
who want to run higher or lower values.

## Verification

Focused tests:

```bash
TMPDIR=/data/tmp env CARGO_TARGET_DIR=/data/tmp/cass-target-pageprep6-20260502T2020Z \
  cargo test --lib lexical_rebuild_default_page_prep_worker_parallelism_stays_bounded_without_channel_cap -- --nocapture

TMPDIR=/data/tmp env CARGO_TARGET_DIR=/data/tmp/cass-target-pageprep6-20260502T2020Z \
  cargo test --lib lexical_rebuild_pipeline_settings_snapshot_defaults_page_prep_workers_from_worker_budget -- --nocapture
```

Full required checks:

```bash
TMPDIR=/data/tmp env CARGO_TARGET_DIR=/data/tmp/cass-target-pageprep6-20260502T2020Z cargo check --all-targets
TMPDIR=/data/tmp env CARGO_TARGET_DIR=/data/tmp/cass-target-pageprep6-20260502T2020Z cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
