# cass (coding-agent-session-search)

## Cargo Build Serialization (CRITICAL)

**Never run `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo bench`, or `cargo run` directly.** Use `cargo-guarded` instead — it serializes compilation via flock so concurrent subagents don't thrash the machine, and caps test parallelism to 4 threads.

The guard script lives at `bin/cargo-guarded` in this repo. A PreToolUse hook (`.claude/hooks/cargo-guard.sh`) blocks raw `cargo` compilation commands and tells you to use it. Non-compilation commands (`cargo metadata`, `cargo --version`, `cargo add`, `cargo fmt`) are unaffected.

```bash
# Instead of:
cargo build --release
cargo test
cargo check

# Use:
./bin/cargo-guarded build --release
./bin/cargo-guarded test
./bin/cargo-guarded check
```

Why: This repo has ~6k tests. Uncapped test parallelism (12 threads) saturates all cores and crashes WSL. The guard caps both build jobs (4) and test threads (4). Nextest parallelism is capped separately in `.config/nextest.toml`.
