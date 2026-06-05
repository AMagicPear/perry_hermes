---
name: rust-core-style
description: "Example skill: Rust coding style reminders"
---

# Rust Style

- Prefer `?` over `unwrap`/`expect` in non-test code.
- Use `tracing` for diagnostics, not `println!`.
- Run `cargo clippy --all-targets --all-features -- -D warnings` before committing.