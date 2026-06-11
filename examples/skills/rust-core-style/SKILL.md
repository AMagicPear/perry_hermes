---
name: rust-core-style
description: Prefer small, focused Rust modules with explicit ownership and minimal public surface area.
---

# Rust Core Style

This skill teaches the agent a consistent Rust coding style. Skills are loaded
into the system prompt, so the agent will follow these guidelines automatically
when working on Rust code.

## Principles

- Keep modules narrowly scoped and easy to understand in isolation.
- Prefer explicit ownership and clear data flow over clever abstractions.
- Extract shared behavior only when it is genuinely cross-cutting.

## Naming

- Use `snake_case` for functions, methods, and variables.
- Use `PascalCase` for types, traits, and enums.
- Prefix internal helpers with `_` only when the name would otherwise shadow.

## Error Handling

- Use `anyhow::Result` for application-level errors.
- Use `thiserror` for library crates with typed errors.
- Avoid `.unwrap()` in production code — use `?` or `.context()`.

## Testing

- Place unit tests in a `#[cfg(test)] mod tests` block at the bottom of the file.
- Use `ScriptedProvider` or mock traits for deterministic tests.
- Name test functions as `test_<what>_<condition>_<expected>`.
