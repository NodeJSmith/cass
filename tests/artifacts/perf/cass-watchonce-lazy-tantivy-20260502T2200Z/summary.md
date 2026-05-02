# cass watch-once / Tantivy summary profiling, 2026-05-02

## Workload

Binary target:

```bash
TMPDIR=/data/tmp CARGO_TARGET_DIR=/data/tmp/cass-target-watchonce-lazy-20260502T2200Z cargo build --profile profiling --bin cass
```

Original probe:

```bash
/data/tmp/cass-target-watchonce-lazy-20260502T2200Z/profiling/cass index --watch-once /home/ubuntu/.codex/sessions/2026/05/02/rollout-2026-05-02T07-34-40-019de878-1fb0-7ad1-b7b0-b8c9a80769fa.jsonl --json
```

## Results

The prior lazy-Tantivy-open patch did not materially move the zero-conversation
watch-once probe:

| run | elapsed_ms | wall | max RSS |
| --- | ---: | ---: | ---: |
| before | 3302 | 3.42s | 1,434,964 KB |
| after lazy open | 3202 | 3.36s | 1,454,280 KB |

Current-head behavior changed after later watch-once commits and the same input
now indexes real history rather than producing a zero-conversation probe. The
`meta-fastpath` run was stopped after cass emitted its own stall detector event:

| run | result | wall before stop | max RSS | note |
| --- | --- | ---: | ---: | --- |
| meta-fastpath | terminated after stall event | 3:03.78 | 2,870,700 KB | `current=120`, `total=917`, `stall_elapsed_ms=120090` |

Stack capture for the stalled process was blocked by host policy:

```text
gdb: ptrace blocked by /proc/sys/kernel/yama/ptrace_scope
/proc/<pid>/stack: Permission denied
```

The actionable improvement kept from this pass is narrower: avoid opening the
full Tantivy index when `searchable_index_summary` only needs doc/segment counts
and `meta.json` has no delete bitsets. On the large existing tokenizer-hotspot
index at `/home/ubuntu/cass-post-tokenizer-hotspot-20260502T035907Z`, current
`cass status --json` with the new fast path completed in:

| command | wall | max RSS |
| --- | ---: | ---: |
| `cass status --data-dir /home/ubuntu/cass-post-tokenizer-hotspot-20260502T035907Z --json --color=never` | 0.01s | 23,668 KB |

Artifacts:

- `before.out.json`, `before.stderr.txt`
- `after.out.json`, `after.stderr.txt`
- `meta-fastpath.out.json`, `meta-fastpath.stderr.txt`
- `status-meta-fastpath.out.json`, `status-meta-fastpath.stderr.txt`
