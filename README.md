## rust-dtf

Mini deterministic task framework core for durable-style orchestrations.

Notes
- Crate name is hyphenated: `rust-dtf`. In Rust code, import it as `rust_dtf`.
- This repo currently provides a library crate with a minimal orchestration runtime and an integration test.

Install (when published)
```toml
[dependencies]
rust-dtf = "0.1"
```

Use in code (import path underscores the crate name)
```rust
use rust_dtf::{OrchestrationContext, Executor};
```

Local development
- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`

Project layout
- `src/lib.rs` — orchestration primitives and executor
- `tests/mini_dtf.rs` — integration test demonstrating orchestration and deterministic replay
