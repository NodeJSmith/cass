# cass (coding-agent-session-search)

## Cargo Build Serialization (CRITICAL)

**Never run `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo bench`, or `cargo run` directly.** Use `cargo-guarded` instead — it serializes compilation via flock so concurrent subagents don't thrash the machine.

```bash
# Instead of:
cargo build --release
cargo test
cargo check

# Use:
cargo-guarded build --release
cargo-guarded test
cargo-guarded check
```

A PreToolUse hook blocks raw `cargo` compilation commands and tells you to use `cargo-guarded`. Non-compilation commands (`cargo metadata`, `cargo --version`, `cargo add`, `cargo fmt`) are unaffected.

Why: Rust compilation is extremely CPU/memory-intensive. Concurrent builds pin all cores and can crash WSL.
