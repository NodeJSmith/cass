# cass shard-plan tail estimate perf slice

Date: 2026-05-04
Workload: `cass index --watch-once /home/ubuntu/.codex/sessions/2026/05/02/rollout-2026-05-02T18-41-41-019deada-cd88-74e3-b215-90094437fbc0.jsonl --data-dir <fresh> --json --progress-interval-ms 5000 --color=never`
Binary: `/data/tmp/cass-target-next-perf-20260504/profiling/cass`

## Result

| Build | CLI elapsed | Wall time | Max RSS | FS outputs | `plan_lexical_shards` |
| --- | ---: | ---: | ---: | ---: | ---: |
| Accepted baseline: final-frontier tail budget repeat | 40,427 ms | 41.53 s | 40,473,608 KB | 14,719,024 | 2,857 ms |
| Candidate: planner from tail high-water estimates | 38,029 ms | 39.13 s | 40,364,176 KB | 13,715,192 | 850 ms |

End-to-end speedup vs accepted baseline: 1.063x CLI elapsed, 1.061x wall time.

## Lever

`list_conversation_footprints_for_lexical_rebuild` previously streamed every `messages` row to count per-conversation message totals before authoritative lexical repair could start. That pass was only a shard-sizing heuristic; exact document and message accounting happens later when the rebuild packet pipeline reads the conversation/message content.

The new path reads `conversations` plus the hot `conversation_tail_state` cache and estimates each footprint from `last_message_idx + 1`. This keeps shard planning proportional to conversations instead of retained message history and avoids a second full `messages` walk before the real rebuild.

## Behavior proof

- Shard `message_count` is still rewritten to `LEXICAL_SHARD_UNKNOWN_MESSAGE_COUNT` before validation, so the planner estimate is not a validation contract.
- Sparse-index fixture coverage now proves the estimate semantics: a single message at `idx = 10` plans as 11 estimated message slots.
- Targeted proof command passed:
  `env CARGO_TARGET_DIR=/data/tmp/cass-target-next-perf-20260504 cargo test --lib list_conversation_footprints_for_lexical_rebuild_estimates_bytes_and_keeps_empty_conversations -- --nocapture`

## Raw evidence

- `shard-plan-tail-estimate.out.json`
- `shard-plan-tail-estimate.stderr.txt`
- `shard-plan-tail-estimate.time.txt`
