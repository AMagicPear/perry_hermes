# Dead Code and Redundancy Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce dead code, orphan `pub` items, redundant logic, and unused dependencies across the workspace through five focused passes, each gated by `cargo test --workspace` green and `cargo clippy --all-targets --all-features -- -D warnings` clean.

**Architecture:** Sequential passes (1 → 5), each with its own discovery method, deletion rule, and verification gate. The depth-of-understanding rule from the spec governs every deletion: read context first, write a one-sentence "why safe" in the commit body, never delete on tool verdict alone.

**Tech Stack:** Rust workspace (5 crates), `cargo`, `cargo machete`, `cargo +nightly udeps` (optional), `rg` (ripgrep), `cargo clippy`, `cargo doc`.

**No new dependencies.** No new files. No behavior changes. No test additions. Deletions only.

---

## File Structure

### Modified (one or more files per pass)

Each pass touches different files; the actual list is determined during execution. Expected hot files based on the spec:

| Crate | Likely modified |
|---|---|
| `hermes-core` | `lib.rs` (re-exports), `accumulator.rs`, `provider.rs` |
| `hermes-providers` | `openai.rs`, `anthropic.rs`, `lib.rs` (re-exports) |
| `hermes-skill-loader` | `lib.rs`, `layout.rs`, `frontmatter.rs` |
| `hermes-agent` | `lib.rs` (re-exports), `loop_engine.rs`, `runtime_agent.rs`, `config.rs`, `context/compressor.rs`, `tools/*` |
| `hermes-cli` | `tui/render.rs`, `tui/app.rs`, `tui/run.rs`, `tui/input.rs`, `main.rs` |
| `Cargo.toml` (workspace + per-crate) | `[dependencies]`, `[dev-dependencies]`, `[features]` |

### Deleted

A variable number of items: `pub fn`/`pub struct`/`pub enum`/`pub const`/unused fields/whole test helpers, as discovered by the passes. No whole files are deleted in this plan (the `rust_out` binary and other repo-root cruft are out of scope per the spec).

---

## Pre-Task 0: Baseline

**Files:** none

- [ ] **Step 1: Record baseline line count and dependency counts**

```bash
find crates -name '*.rs' -not -path '*/target/*' -exec wc -l {} + | tail -1
# note the total
grep -c '^\[' crates/*/Cargo.toml | head
# per-crate dep block counts (rough indicator)
```

- [ ] **Step 2: Verify clean starting state**

```bash
cargo test --workspace 2>&1 | tail -20
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10
```

Expected: both green. If not, the spec does not authorize fixing pre-existing failures — stop and report to the user.

- [ ] **Step 3: Verify tooling availability**

```bash
which rg cargo
# Try cargo-machete; install if missing
cargo machete --version 2>/dev/null || cargo install cargo-machete
# Probe for nightly
rustup toolchain list | grep nightly || echo "no nightly"
```

- [ ] **Step 4: Create a working branch**

```bash
git checkout -b cleanup/dead-code-and-redundancy
```

---

## Task 1: Pass 1 — Unused Dependencies

**Files:**
- Modify: `Cargo.toml` (workspace), `crates/*/Cargo.toml` (per-crate)
- Tool: `cargo machete`, optionally `cargo +nightly udeps`

Crate order: leaves first (`hermes-core`, `hermes-providers`, `hermes-skill-loader`), then `hermes-agent`, then `hermes-cli`. This minimizes cross-crate compile errors.

### Sub-task 1.1: `hermes-core`

- [ ] **Step 1: Run machete on the crate**

```bash
cargo machete --skip-target-dir 2>&1 | tee /tmp/machete-core.log
```

- [ ] **Step 2: For each candidate dep, run final rg confirmation**

For each line in `/tmp/machete-core.log` referencing `hermes-core`:

```bash
rg -n '\b<dep_name>\b' crates/hermes-core/
# 0 hits in non-Cargo.toml files = confirmed unused
```

If the dep is a `build-dependency` or used in a `[[bin]]`/`[[example]]`/target-conditional block, do **not** delete (machete has known false positives there). Re-verify by reading the `[build-dependencies]` block, `[features]`, and any `#[cfg(...)]` usage.

- [ ] **Step 3: Remove confirmed unused deps**

Edit `crates/hermes-core/Cargo.toml`. Remove the line. Re-run:

```bash
cargo test -p hermes-core 2>&1 | tail -20
```

Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-core/Cargo.toml Cargo.lock
git commit -m "chore(cleanup): drop unused deps from hermes-core (pass 1)

Removed: <list>
Verified: cargo test -p hermes-core green; cargo clippy -p hermes-core
--all-targets -- -D warnings clean."
```

### Sub-task 1.2–1.5: `hermes-providers`, `hermes-skill-loader`, `hermes-agent`, `hermes-cli`

Repeat Sub-task 1.1's pattern for each crate. One commit per crate. For each:

- [ ] `cargo machete` → capture log
- [ ] rg-confirm each candidate
- [ ] edit `Cargo.toml`, drop the line
- [ ] `cargo test -p <crate>` green
- [ ] `cargo clippy -p <crate> --all-targets -- -D warnings` clean
- [ ] commit with the format from 1.1

### Sub-task 1.6: dev-dependencies sweep

For crates whose dev-deps include anything that machete flagged and Step 1.1–1.5 didn't already address, repeat the cycle targeting `[dev-dependencies]`. If a nightly toolchain is available, supplement with:

```bash
cargo +nightly udeps --workspace --all-targets 2>&1 | tee /tmp/udeps.log
```

For each line, treat it as a machete hit and re-verify with rg.

- [ ] commit per crate as before

### Sub-task 1.7: Pass 1 verification

- [ ] Run the full gate:

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

Expected: both green.

- [ ] Record Pass 1 line count delta in a scratch file (used at the end):

```bash
find crates -name '*.rs' -not -path '*/target/*' -exec wc -l {} + | tail -1
```

---

## Task 2: Pass 2 — Compiler-Detected Dead Code

**Files:** `crates/*/src/**/*.rs` (whichever has warnings)

Tool: compiler warnings from `cargo build` and `cargo test --no-run`.

Crate order: same as Pass 1.

### Sub-task 2.1: collect all warnings

- [ ] **Step 1: Capture build warnings**

```bash
cargo build --workspace --all-targets 2>&1 | tee /tmp/build-warnings.log
cargo test --workspace --no-run 2>&1 | tee -a /tmp/build-warnings.log
```

- [ ] **Step 2: Filter to relevant lints**

```bash
rg 'warning: (unused_imports|unused_variables|dead_code|unused_assignments|unused_must_use|unreachable_code|unreachable_patterns)' /tmp/build-warnings.log
```

If a warning is on a `pub` item, defer to Pass 3 (the compiler doesn't see cross-crate uses).

### Sub-task 2.2: per-warning deletion

For each warning, the per-warning workflow is:

- [ ] **Step 1: Read the file and 30+ lines of context around the warning**

```bash
# locate the line from the warning
rg -n '<symbol_name>' crates/<crate>/src/<file>.rs
```

Read the file. Confirm:
- The item is not referenced via reflection, trait object, `format!`/`{}` interpolation, or `serde` derive magic.
- The unused variable is not the result of a `#[derive(...)]` that needs a specific field name (rare, but happens with `StructOpt`/`clap`).
- The dead function is not a `#[no_mangle]` extern or a `#[link_section]` symbol.

- [ ] **Step 2: Delete the item**

Use Edit tool to remove the offending code. If the unused import is the entire use statement, remove the line. If it's one symbol in a multi-symbol `use`, narrow the `use` to only the used symbols.

- [ ] **Step 3: Run the per-crate test gate**

```bash
cargo test -p <crate> 2>&1 | tail -10
```

Expected: green.

- [ ] **Step 4: Commit (logical batch)**

Group related deletions into one commit (e.g. "all unused imports in hermes-core/error.rs"). Commit body must list each removed item and the one-sentence justification.

```bash
git add crates/<crate>/src/<file>.rs
git commit -m "chore(cleanup): drop compiler-detected dead code in <crate>

Pass 2. Removed:
- <file>:<line> <symbol>: <one-sentence why safe>
- ...

Verified: cargo test --workspace green; cargo clippy --all-targets
--all-features -- -D warnings clean."
```

### Sub-task 2.3: Pass 2 verification

- [ ] Re-run:

```bash
cargo build --workspace --all-targets 2>&1 | rg 'warning' | head
```

Expected: empty (or only `pub`-item warnings deferred to Pass 3).

- [ ] Full gate:

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

- [ ] Record line count delta.

---

## Task 3: Pass 3 — Workspace-grep Pub Surface Audit (highest-leverage)

**Files:** all `crates/*/src/**/*.rs`, especially `lib.rs` (re-exports), `loop_engine.rs`, `runtime_agent.rs`, `tui/render.rs`, `anthropic.rs`

Crate order: same as Pass 1. The first crate to do is `hermes-core` (its pub surface is depended on by every other crate).

### Sub-task 3.0: enumeration per crate

For each crate:

- [ ] **Step 1: Generate the pub-item list**

```bash
rg -n '^\s*pub\s+(fn|struct|enum|trait|const|static|type|use|mod)\b' crates/<crate>/src/ > /tmp/pub-<crate>.log
wc -l /tmp/pub-<crate>.log
```

- [ ] **Step 2: For each pub item, search the rest of the workspace**

For each line in `/tmp/pub-<crate>.log`:

```bash
# Extract the item name (the second whitespace-separated word)
ITEM=$(echo "<line>" | awk '{print $2}' | sed 's/[<(].*//')
rg -n "\b${ITEM}\b" crates/ --glob '!crates/<this_crate>/src/<owning_file>'
```

A pub item is **orphan** if the search returns 0 hits outside its own file.

- [ ] **Step 3: Read context before deciding**

For each orphan candidate, do the depth-of-understanding check from the spec:

1. Read 50+ lines of context around the definition.
2. Check git blame: `git log --all --oneline -- crates/<crate>/src/<file>.rs | head -20`. If the item was added as part of a removed feature, the commit message will say so.
3. If the item is on a `pub trait`, check whether other methods on that trait are referenced; trait methods must move together.
4. If the item is a `Default` / `Display` / `From` / `Serialize` impl, grep for the trait's full path: `rg 'impl Display for <Type>'` and `rg '<Type>: Display'`.
5. If the item is referenced in `docs/`, `README.md`, or `examples/config/*.toml`, prefer demoting to `pub(crate)` over deletion.

- [ ] **Step 4: Make the call**

Three outcomes:
- **Delete** (if it's truly orphan and the read confirms no callers via reflection/macros)
- **Demote to `pub(crate)`** (if it's used only inside the same crate)
- **Keep** (if it's referenced in docs/examples or reflection/serde machinery demands it)

### Sub-task 3.1–3.5: per-crate execution

For each crate (`hermes-core`, `hermes-providers`, `hermes-skill-loader`, `hermes-agent`, `hermes-cli`):

- [ ] Run Sub-task 3.0
- [ ] Apply the deletions/demotions in **logical groups** (e.g. "remove orphan loop events", "shrink agent pub re-exports", "demote tui render helpers to pub(crate)")
- [ ] Per group: one commit

```bash
git add crates/<crate>/src/<file>.rs
git commit -m "refactor(cleanup): drop orphan pub surface in <crate>

Pass 3. Changes:
- <file>:<line> removed <symbol>: <one-sentence why safe>
- <file>:<line> demoted <symbol> to pub(crate): <why>
- ...

Verified: cargo test --workspace green; cargo clippy --all-targets
--all-features -- -D warnings clean; cargo run --example <name>
for every example still succeeds."
```

- [ ] After the crate's commits, run examples:

```bash
cargo run --workspace --examples 2>&1 | tail -20
```

(Or build-only if examples need API keys: `cargo build --workspace --examples`.) Examples that no longer compile after a `pub` removal signal a missed pub item; the removal was wrong — revert and investigate.

### Sub-task 3.6: Pass 3 verification

- [ ] Full gate:

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
cargo doc --workspace --no-deps 2>&1 | tail -10
```

- [ ] Record line count delta.

---

## Task 4: Pass 4 — Redundant Logic

**Files:** wherever clippy points

- [ ] **Step 1: Run clippy with the targeted lint set**

```bash
cargo clippy --workspace --all-targets --all-features -- \
  -W clippy::redundant_clone \
  -W clippy::redundant_locals \
  -W clippy::or_fun_call \
  -W clippy::needless_pass_by_value \
  -W clippy::single_match \
  -W clippy::single_match_else \
  -W clippy::redundant_pattern_matching \
  -W clippy::if_let_some_else_none \
  -W clippy::redundant_closure \
  -W clippy::manual_let_else \
  -W clippy::needless_return \
  2>&1 | tee /tmp/clippy-redundant.log
```

- [ ] **Step 2: Filter to actionable hits**

```bash
rg '^warning: clippy::' /tmp/clippy-redundant.log
```

- [ ] **Step 3: For each hit, read context and apply the fix**

For each warning, the per-warning workflow:

1. Open the file and read 30+ lines around the location.
2. Confirm the suggested fix is correct. Common false-positive traps:
   - `redundant_clone` on a value whose `Drop` runs external resources (network connections, file handles, locks).
   - `or_fun_call` on a function with side effects (rare but possible).
   - `redundant_pattern_matching` on a `Result` whose `Err` arm is needed for error propagation.
3. If the fix is correct, apply it.
4. If the fix is wrong, add `#[allow(clippy::<lint>)]` with a one-line comment explaining why.

- [ ] **Step 4: Per-file batch verify and commit**

Group by file. After editing one file:

```bash
cargo test -p <crate> 2>&1 | tail -5
```

Then commit:

```bash
git add crates/<crate>/src/<file>.rs
git commit -m "refactor(cleanup): apply clippy::redundant_* fixes in <file>

Pass 4. Removed: <list with line numbers>.
Verified: cargo test -p <crate> green; cargo clippy --all-targets
--all-features -- -D warnings clean."
```

- [ ] **Step 5: Pass 4 verification**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

- [ ] Record line count delta.

---

## Task 5: Pass 5 — tests/ and examples/ Helpers

**Files:** `crates/*/tests/`, `crates/*/examples/`, plus per-crate `[dev-dependencies]` if any become orphaned

### Sub-task 5.1: tests/ helpers

- [ ] **Step 1: List test helper items**

```bash
rg -n '^\s*pub\s+(fn|struct|enum|const|static)\b' crates/*/tests/ > /tmp/test-helpers.log
wc -l /tmp/test-helpers.log
```

- [ ] **Step 2: For each helper, search the workspace**

For each line in `/tmp/test-helpers.log`:

```bash
ITEM=$(echo "<line>" | awk '{print $2}' | sed 's/[<(].*//')
rg -n "\b${ITEM}\b" crates/ --glob '!crates/*/tests/<owning_file>'
```

0 hits = orphan helper.

- [ ] **Step 3: Read context and delete or keep**

For each orphan:
1. Read 30+ lines.
2. If the helper is a one-off fixture for a single test that no longer exists, delete.
3. If the helper is used in 1 test, evaluate: is the helper simpler than inlining? If not, inline and delete the helper.
4. If the helper is referenced in test files that pass, but those tests are themselves orphans, delete the test and the helper together.

- [ ] **Step 4: Skip-or-evaluate for `#[ignore]`-marked tests**

```bash
rg -n '#\[ignore' crates/*/tests/
```

For each ignored test:
- If `#[ignore = "reason"]` with a real reason (flaky, requires env var), keep.
- If `#[ignore]` without a reason AND the test runs cleanly when un-ignored, un-ignore and run it.
- If `#[ignore]` without a reason AND the test fails when un-ignored, delete it.

- [ ] **Step 5: Commit**

```bash
git add crates/*/tests/
git commit -m "refactor(cleanup): drop orphan test helpers and ignored tests

Pass 5. Removed: <list>.
Verified: cargo test --workspace green; cargo clippy --all-targets
--all-features -- -D warnings clean."
```

### Sub-task 5.2: examples/ entries

- [ ] **Step 1: List all examples**

```bash
find crates/*/examples -name '*.rs' -not -name 'live_*' | sort
```

`live_*` examples are out of scope (they hit real APIs).

- [ ] **Step 2: For each non-live example, build it**

```bash
cargo build --example <name> -p <crate>
```

- [ ] **Step 3: For each building example, run it (if offline-safe)**

Examples that use `EchoProvider` or are read-only are runnable offline. Examples that hit network/keys are build-only.

```bash
cargo run --example <name> -p <crate>
# or for build-only:
cargo build --example <name> -p <crate>
```

- [ ] **Step 4: Delete examples that fail to build AND have no clear purpose**

For each example that fails to build with the *current* code, check git history:

```bash
git log --oneline -- crates/<crate>/examples/<name>.rs
```

If the example was added as a probe for a feature that has since been folded into the main binary, delete. If the example documents a use case that real users might copy, fix it instead.

- [ ] **Step 5: Commit**

```bash
git add crates/*/examples/
git commit -m "refactor(cleanup): prune dead examples

Pass 5. Removed: <list with crate:examples/name.rs>.
Verified: cargo build --workspace --examples green; remaining
examples still run (or build, for live_* ones)."
```

### Sub-task 5.3: dev-dependency cleanup

- [ ] **Step 1: Re-run machete targeting only dev-deps**

```bash
cargo machete 2>&1 | rg 'dev-dependencies|examples' -A 30
```

- [ ] **Step 2: For each candidate, rg-confirm**

```bash
rg -n '\b<dep_name>\b' crates/<crate>/tests/ crates/<crate>/examples/
```

If 0 hits, remove from `[dev-dependencies]`. If the dep is also used in `[dependencies]`, leave it.

- [ ] **Step 3: Commit per crate**

```bash
git add crates/<crate>/Cargo.toml Cargo.lock
git commit -m "chore(cleanup): drop now-orphaned dev-deps after pass 5

Removed: <list>.
Verified: cargo test -p <crate> green."
```

### Sub-task 5.4: Pass 5 verification

- [ ] Full gate:

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

- [ ] Record line count delta.

---

## Task 6: Final Verification and Wrap-up

- [ ] **Step 1: Run all gates one more time**

```bash
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
cargo build --workspace --all-targets --examples
```

All four must be green.

- [ ] **Step 2: Compute the line count delta**

```bash
find crates -name '*.rs' -not -path '*/target/*' -exec wc -l {} + | tail -1
# Compare to Pre-Task 0 baseline
```

- [ ] **Step 3: Compute the dependency count delta**

```bash
# Per-crate dep block counts (rough)
grep -c '^[a-zA-Z0-9_-]*\s*=' crates/*/Cargo.toml
# Compare to baseline
```

- [ ] **Step 4: Final wrap-up commit (if any formatting/clippy cleanups accumulated)**

```bash
cargo fmt --all
git diff
# If non-empty, commit:
git commit -am "chore(cleanup): cargo fmt + final clippy cleanup"
```

If empty, no commit needed.

- [ ] **Step 5: Write the final summary commit body**

The last substantive commit (Pass 5) already has a body. Append a final summary to that commit's body via `git commit --amend` (since we are the only author on this branch), or include the deltas in Pass 5's final commit body before pushing:

```
---
Cleanup totals:
- src line count: <before> → <after> (<delta>)
- workspace dep entries: <before> → <after> (<delta>)
- pub items removed: <N>
- pub items demoted: <N>
```

- [ ] **Step 6: Merge to main**

The work was on `cleanup/dead-code-and-redundancy` (created in Pre-Task 0). Fast-forward merge:

```bash
git checkout main
git merge --ff-only cleanup/dead-code-and-redundancy
git branch -d cleanup/dead-code-and-redundancy
```

- [ ] **Step 7: Final report to user**

Briefly summarize:
- Total line count delta
- Total dep entries delta
- Approximate pub items removed/demoted
- Any items skipped (with reason)
- Verification command outputs (just the tail lines: "test result: ok" and "0 warnings emitted")
