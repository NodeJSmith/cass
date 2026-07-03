# Jina Embedder Cleanup Brief

## Context

We added a Jina v2 small ONNX embedder to cass, replacing MiniLM as the default. The integration works (builds, loads model, embeds correctly) but the wiring is messy — jina metadata is scattered across multiple dispatch tables that must stay in sync by hand. This brief covers centralizing the embedder resolution through the registry and fixing review findings.

## Problem

Embedder resolution flows through `FastEmbedder`'s static methods (`canonical_name`, `config_for`, `model_dir_for`) which are hardcoded match tables designed for the frankentorch-native models. We shoehorned jina into these tables even though FastEmbedder can never actually load it — creating parallel metadata that will drift.

## Change 1: Centralize resolution in EmbedderRegistry

Move all embedder dispatch logic from `FastEmbedder`'s static methods into `RegisteredEmbedder` / `EmbedderRegistry`. After this, FastEmbedder is a pure loader with no dispatch tables.

### Add fields to `RegisteredEmbedder`

File: `src/search/embedder_registry.rs`, struct at line ~56

Add two fields:
```rust
pub dir_name: &'static str,        // e.g. "all-MiniLM-L6-v2", "jina-embeddings-v2-small-en"
pub aliases: &'static [&'static str], // e.g. &["fastembed", "all-minilm-l6-v2", "minilm-384"]
```

Update each entry in the `EMBEDDERS` static array with the correct values. Source the aliases from `FastEmbedder::canonical_name`'s current match arms, and dir_name from `FastEmbedder::model_dir_for`.

### Add methods to `RegisteredEmbedder`

```rust
// Replace FastEmbedder::model_dir_for — use self.dir_name
pub fn model_dir(&self, data_dir: &Path) -> Option<PathBuf> {
    if !self.requires_model_files { return None; }
    Some(data_dir.join("models").join(self.dir_name))
}

// Replace FastEmbedder::runtime_model_dir_for — honor env override
pub fn runtime_model_dir(&self, data_dir: &Path) -> Option<PathBuf> {
    model_dir_override().or_else(|| self.model_dir(data_dir))
}
```

### Update `EmbedderRegistry::get()` to use aliases

Currently at line ~262, `get()` calls `FastEmbedder::canonical_name(name)` to normalize before lookup. Replace with alias search on the registry entries themselves:

```rust
pub fn get(&self, name: &str) -> Option<&'static RegisteredEmbedder> {
    let lower = name.trim().to_ascii_lowercase();
    EMBEDDERS.iter().find(|e| {
        e.name == lower
            || e.id == lower
            || e.aliases.iter().any(|a| *a == lower)
    })
}
```

### Migrate callsites (21 total)

Each callsite replaces a `FastEmbedder::` static call with a registry lookup:

| Callsite | File:Line | Current | Replacement |
|---|---|---|---|
| canonical_name | `embedder_registry.rs:271` | `FastEmbedder::canonical_name(name)` | Use alias search above (already in `get()`) |
| canonical_name | `model_manager.rs:472` | normalize name | `registry.get(name)?.name` |
| canonical_name | `model_manager.rs:611` | normalize name | `registry.get(name)?.name` |
| canonical_name | `model_manager.rs:738` | policy embedder | `registry.get(name)?.name` |
| canonical_name | `asset_state.rs:1029` | policy model dir | `registry.get(name)?.runtime_model_dir(data_dir)` |
| canonical_name | `asset_state.rs:1034` | embedder_id → model dir | `registry.get(id)?.runtime_model_dir(data_dir)` |
| canonical_name | `query.rs:4603` | progressive shard | `registry.get(id)` then `get_embedder()` |
| canonical_name | `lib.rs:22630` | env var resolve | `registry.get(&value).map(|e| e.name)` |
| canonical_name | `lib.rs:97276` | quality tier resolve | `registry.get(name)?.name` |
| canonical_name | `lib.rs:97340` | backfill validate | `registry.get(name).is_some()` |
| canonical_name | `lib.rs:97447` | backfill manifest | `registry.get(name)?.name` |
| config_for | `daemon/worker.rs:147` | get embedder_id | `registry.get(name)?.id` |
| config_for | `model_manager.rs:473` | get embedder_id | `registry.get(name)?.id` |
| config_for | `model_manager.rs:612` | get embedder_id | `registry.get(name)?.id` |
| model_dir_for | `embedder_registry.rs:114` | delegate to FastEmbedder | Use `self.dir_name` directly |
| runtime_model_dir_for | `embedder_registry.rs:346` | validate files | Use `self.runtime_model_dir()` |
| runtime_model_dir_for | `model_manager.rs:478` | probe availability | registry lookup |
| runtime_model_dir_for | `model_manager.rs:620` | load context | registry lookup |
| runtime_model_dir_for | `asset_state.rs:1030,1035` | model dir | registry lookup |
| runtime_model_dir_for | `lib.rs:96443` | models status | registry lookup |
| runtime_model_dir_for | `lib.rs:96722` | install dir | registry lookup |

### Remove jina from FastEmbedder

After migration, remove the `"jina"` arms from:
- `FastEmbedder::canonical_name` (line ~152)
- `FastEmbedder::config_for` (line ~180)
- `FastEmbedder::model_dir_for` (line ~127)

FastEmbedder becomes a pure loader for frankentorch-native models only, matching its module doc.

## Change 2: Fix `cass models install jina`

The CLI model management path doesn't know about jina.

### Add jina to `resolve_cli_model_name`

File: `src/lib.rs`, function at ~line 96680

Add `"jina"` to the match arms so `cass models install jina` / `cass models status jina` work.

### Add jina to `run_models_backfill` known list

File: `src/lib.rs`, `run_models_backfill` function — add `"jina"` to the `known` array.

### Update test `every_resolved_canonical_name_has_manifest_and_dir_mapping`

File: `src/lib.rs`, test at ~line 98868 — add `"jina"` to the test's expected-canonical-names list.

## Change 3: Use local trait aliases in onnx_embedder.rs

File: `src/search/onnx_embedder.rs`

Change:
```rust
use frankensearch::{ModelCategory, SearchError, SearchResult, SyncEmbed};
```
To:
```rust
use super::embedder::{Embedder, EmbedderError, EmbedderResult};
use frankensearch::ModelCategory;
```

And `impl SyncEmbed for JinaEmbedder` → `impl Embedder for JinaEmbedder`, matching the convention in `hash_embedder.rs` and `fastembed_embedder.rs`.

## Change 4: Update stale comments and docs

- `src/search/fastembed_embedder.rs` module doc (lines 1-20): update to clarify it's the frankentorch-native backend only, no longer the sole embedding path
- `src/search/embedder_registry.rs` module doc (lines 16-22): add jina to the documented embedder list
- `src/main.rs` (~line 225-230): update or remove the "no ONNX Runtime hazard" comment — ONNX is back via ort
- `Cargo.toml` (lines 84-90): update the comment block that says "ONNX-Runtime stack was removed"

## Change 5: Fix module ordering

File: `src/search/mod.rs`

Move `pub mod onnx_embedder;` to alphabetical position (between `model_manager` and `pack_planner`).

## Change 6: Magic number

File: `src/search/onnx_embedder.rs`, line ~59

Extract `with_intra_threads(4)` to a named constant:
```rust
const ONNX_INTRA_THREADS: usize = 4;
```

## Change 7: Unit tests for mean_pool_and_normalize

File: `src/search/onnx_embedder.rs`

Add a `#[cfg(test)] mod tests` block covering `mean_pool_and_normalize`:
- All-ones mask (uniform pooling)
- Partial mask (some tokens masked out)
- All-zero mask (edge case — should return zero vector)
- Normalization check (output should be unit length)

For the full ONNX load/embed path, add an `#[ignore]` integration test gated on a model-dir env var, matching the pattern in `fastembed_embedder.rs`.

## Change 8: Update RuntimeLoadability probe comment

File: `src/search/model_acquisition.rs`, line ~113-116

The `probe_cheap_host()` function hardcodes `cpu_has_avx2 = true` with a comment saying that's safe because "the pure-Rust frankentorch embedder has no ONNX and no AVX requirement." That comment is now wrong since ort is back. Update it to note that AVX2 is required when the onnx-embedder feature is active, and that all target machines have AVX2.

## Verification

After all changes:
1. `cargo build --release` succeeds
2. `cass models install jina` downloads model files
3. `cass models status --json` shows jina as active/installed
4. `cass index --semantic` (defaults to jina) creates index-jina-v2-small-512.fsvi
5. `cass search --mode semantic "test query"` loads the jina index and returns results
