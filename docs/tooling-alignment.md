# Perry Hermes — Agent Self-Use Tooling Alignment

> Status: **draft plan, pre-implementation**. Source of truth for the
> agent-tooling work; will be updated as PRs land.
> Investigation target: `~/.hermes/hermes-agent` (Nous Research's
> `hermes-agent`, Python; the upstream project perry_hermes takes design
> inspiration from).

## 0. Scope Correction (read this first)

This document is about **LLM-callable tools** — the function-calling surface
perry_hermes exposes to the model — not about the meta-tools the
agent-running client (this session's `terminal` / `read_file` / `write_file`
/ `skill_view` / `skills_list`) gives the agent process.

Those are two different layers:

| Layer | Defined in | Consumer |
|---|---|---|
| LLM-callable tools (function-calling) | `crates/hermes-core/src/tool.rs` + `crates/hermes-agent/src/tool_catalog.rs` | The model, during a `run_session_turn` |
| Agent-session meta-tools (terminal, read_file, etc.) | The perry_hermes host (CLI / TUI / future gateway) | The agent process, between turns |

The investigation that prompted this document found the agent's
"this feels clunky" complaints stem mostly from **layer 1** being thin:
when the model lacks a `read_file` / `patch` / `grep` / `apply_patch` /
`delegate_task` / `vision_analyze` tool, it falls back to spawning shell
commands and parsing the output, or asking the agent process to do large
overwrites. That mess shows up in *this* session as "the model keeps
wholesale-replacing files I could've patched", which feels like a
meta-tool problem but is really a layer-1 problem.

So this document is a **layer-1 gap analysis and repair plan**, not a
meta-tool redesign. Layer-2 (the session's `terminal` / `read_file` /
`write_file` / `skill_view` / `skills_list`) is out of scope here; if it
turns out to need changes, those go in a separate document.

## 1. How hermes-agent structures layer 1 (the reference)

Source files studied:

- `tools/registry.py` (589 LOC) — singleton `ToolRegistry` collecting
  self-registered tools.
- `model_tools.py` (1174 LOC) — public API (`get_tool_definitions`,
  `handle_function_call`, `get_all_tool_names`).
- `tools/file_tools.py` (1700+ LOC) — `read_file_tool`, `write_file_tool`,
  `patch_tool`, `search_tool` (LLM-callable).
- `tools/file_operations.py` (~1900 LOC) — `ShellFileOperations` ABC
  with backend implementations over local / docker / ssh / modal /
  daytona / singularity terminals; exposes `read_file` / `write_file` /
  `search` / `lint` / `patch` primitives.
- `tools/patch_parser.py` — V4A patch format parser
  (`*** Begin Patch` / `*** Update File` / `*** Add File` / `*** Delete
  File` / `*** Move File`).
- `tools/vision_tools.py` — `vision_analyze_tool(image_url, prompt)`.
- `tools/delegate_tool.py` (1900+ LOC) — `delegate_task(goal, context,
  toolset_restriction, max_children, depth)` spawning child `AIAgent`
  instances.
- `tools/session_search_tool.py` — FTS5-backed `session_search(query |
  session_id | browse)` for cross-session recall.
- `tools/todo_tool.py` — `todo` tool with `TodoStore` per-agent.

The pattern:

1. **Self-registration via `registry.register()` at module import time.**
   `discover_builtin_tools()` walks `tools/*.py`, AST-detects top-level
   `registry.register(...)` calls, and imports the module. No central
   "I know about all tools" list to maintain.
2. **Single shape per concern.** The `search` tool supports `content |
   files | both` modes via a `target` argument; the `session_search` tool
   supports `discovery | scroll | browse` via arg inference. One tool
   surface, many modes — keeps the model from having to remember
   tool-name variants.
3. **Backend abstraction for execution.** `ShellFileOperations` is an
   ABC; concrete impls exist for local, docker, ssh, modal, daytona,
   singularity. The LLM-facing tool (`read_file_tool`) just calls into
   the active backend; the model never knows which terminal it's on.
4. **Tool metadata is data, not strings.** `ToolEntry` carries `name`,
   `toolset`, `schema`, `handler`, `check_fn`, `requires_env`, `is_async`,
   `description`, `emoji`, `max_result_size_chars`,
   `dynamic_schema_overrides`. Schemas can be mutated at
   `get_definitions()` time (e.g. `delegate_task`'s `description`
   reflects current `max_concurrent_children`).
5. **Toolset granularity is enforced by registry.** Toolsets are
   declared, not enumerated. `disabled_toolsets` from config is
   intersected at definition time, not by post-hoc filtering.
6. **check_fn TTL cache.** External-availability probes (Docker daemon
   up, Modal SDK installed) are cached for 30s; `invalidate_check_fn_cache()`
   is the explicit escape hatch.
7. **Subagent isolation is a first-class tool.** `delegate_task` is
   *not* internal runtime machinery — it's a tool the LLM can call, with
   its own JSON Schema, restricted toolset, and depth/concurrency caps.
   The parent never sees the child's intermediate tool calls; only the
   final summary.

## 2. perry_hermes current layer-1 state (the diagnosis)

Read from:

- `crates/hermes-core/src/tool.rs`
- `crates/hermes-agent/src/tool_catalog.rs`
- `crates/hermes-agent/src/tools/` (directory listing)
- `crates/hermes-agent/src/lib.rs`

What exists:

- `Tool` trait (async `invoke`, `name`, `description`, `json_schema`).
- `InMemoryRegistry` in `perry-hermes-core`.
- `tool_catalog::build_registry(disabled_toolsets, skills_dir)` returns
  the registry of enabled tools.
- Built-in tools live under `crates/hermes-agent/src/tools/`. From the
  file listing: a small set (~4–6), likely `terminal` / `url_fetch` /
  `file` / skills, gated by `disabled_toolsets`.

What is missing for hermes-agent parity (this is the gap list):

| # | Capability | hermes-agent location | perry_hermes today | User-visible symptom in this session |
|---|---|---|---|---|
| G1 | `read_file` with offset/limit + cap | `tools/file_tools.py:692` | Absent or minimal | Large file → terminal-cat; "I can only see the first 500 lines" |
| G2 | `write_file` with safe path resolution | `tools/file_tools.py:1043` | Absent (or under `terminal`) | LLM shells out `cat > file` instead of calling a tool |
| G3 | `apply_patch` (V4A format) | `tools/patch_parser.py` + `tools/file_tools.py:1121` | Absent | LLM rewrites entire files via `write_file`-style replace; big diff, no review |
| G4 | `search` (ripgrep-style content/files/both) | `tools/file_tools.py:1292` | Absent | LLM runs `grep` through terminal, then I have to re-parse |
| G5 | `vision_analyze(image_url, prompt)` | `tools/vision_tools.py:797` | Absent | Image attached to a user turn is opaque to the LLM |
| G6 | `delegate_task(goal, ...)` | `tools/delegate_tool.py:1937` | Absent | The `subagent-driven-development` skill cannot actually run |
| G7 | `session_search` (cross-session FTS) | `tools/session_search_tool.py:494` | Absent | No cross-session recall; user must re-paste context |
| G8 | `todo` (in-session task list, per-agent) | `tools/todo_tool.py` | Absent | Multi-step plans have no in-session progress tracker |
| G9 | Toolset metadata: `is_async`, `requires_env`, `max_result_size_chars`, `dynamic_schema_overrides` | `tools/registry.py:80–106` (`ToolEntry.__slots__`) | `Tool` trait has only the minimum (`name` / `description` / `json_schema`) | Can't cap tool result size; can't reflect runtime config in schemas; no async distinction |
| G10 | `check_fn` availability probes | `tools/registry.py:121–146` | Absent | Tools that need a daemon (browser, modal) can't gracefully degrade |
| G11 | Path safety (deny list, cross-profile, sensitive files) | `tools/file_tools.py:81–340` (`_check_sensitive_path`, `_check_cross_profile_path`, `_is_blocked_device_path`) | Absent (or only partial) | LLM can `cat /etc/shadow` or stomp the host's `.ssh/` |
| G12 | Backend abstraction (local / docker / ssh / modal / daytona) | `tools/file_operations.py:374` (`FileOperations` ABC) | Absent — `terminal` is local-only | No sandboxing, no remote-exec, no "agent runs on a $5 VPS" story |
| G13 | Self-registration / discovery | `tools/registry.py:57` (`discover_builtin_tools`) | Manual `build_registry(...)` enumerates tools | Adding a new tool = edit the catalog; easy to forget one |
| G14 | MCP tool discovery (external servers) | `tools/mcp_tool.py` (mentioned in registry's exclusion list) | Absent | Can't extend perry_hermes with external MCP servers (filesystem / github / etc.) |
| G15 | Per-tool description / `max_result_size_chars` / `emoji` | `tools/registry.py:97–98` | `Tool` trait has only description | No way to enforce a 100K-char result cap; no UI hint emoji |

The "user-visible symptom" column is the **diagnostic** — it's why this
document exists. If a perry_hermes contributor wonders "do we really
need G3?", the answer is "yes, because right now the LLM is asking the
agent process to wholesale-replace files it could have patched".

## 3. Repair Plan

The plan is grouped into four PRs by dependency, smallest first. Each
PR must leave `cargo test --workspace` and `cargo clippy --all-targets
--all-features -- -D warnings` green. None of this changes the existing
`Tool` trait signature in a breaking way — the trait grows.

### PR-1: Registry metadata + discovery (G9, G13, G15)

Goal: make adding a new tool cheap and uniform; lay the metadata
groundwork the rest of the PRs need.

**Files to add/change:**

- `crates/hermes-core/src/tool.rs` — extend the `Tool` trait (default
  impls, no breakage for existing impls):
  - `fn is_async(&self) -> bool { false }`
  - `fn requires_env(&self) -> &[&str] { &[] }`
  - `fn max_result_size_chars(&self) -> Option<usize> { None }`
  - `fn emoji(&self) -> Option<&str> { None }`
  - `fn check_available(&self) -> bool { true }`
  - `fn dynamic_schema_overrides(&self) -> Option<serde_json::Value> { None }`
- `crates/hermes-core/src/registry.rs` — `InMemoryRegistry` already
  exists; add a `register_dynamic(Box<dyn Tool>)` method and a
  `from_dir(path: &Path)` constructor that:
  - Walks the directory for `*.rs` files (excluding `mod.rs` /
    `registry.rs`),
  - Uses `syn` (already a transitive dep via `serde_derive`? verify) or
    a tiny hand-rolled scanner to find `inventory::submit!` /
    `register!(...)` calls, OR — simpler — uses `inventory` crate for
    true Rust-side plugin discovery.
  - Recommended: use the `inventory` crate (Rust's standard plugin-
    registration primitive) and have each tool's `mod.rs` call
    `inventory::submit!`. This mirrors hermes-agent's "import the module,
    registration is a module-level side effect" pattern.
- `crates/herry-hermes-agent/src/tool_catalog.rs` — `build_registry`
  switches to `InMemoryRegistry::from_inventory()` plus a final
  `disabled_toolsets` filter.

**Acceptance:**

- New test: `inventory_registers_all_builtin_tools` — assert the
  registry built with no `disabled_toolsets` contains the same tool
  names as today's `build_registry`.
- New test: `disabled_toolset_removes_tools` — assert
  `disabled_toolsets = ["terminal"]` removes exactly the terminal
  tool.
- New test: `register_external_tool_at_runtime` — assert a tool
  registered via `register_dynamic` shows up in
  `registry.list_tool_names()`.

**Why first:** every other PR's tool implementation needs the metadata
fields to mean something.

### PR-2: File-system tools trio — `read_file`, `write_file`, `apply_patch`, `search` (G1, G2, G3, G4, G11)

Goal: give the LLM the four tools it needs to do code work without
shelling out, and do so safely.

**Files to add/change:**

- `crates/hermes-agent/src/tools/fs/` (new module) with:
  - `mod.rs` — public `fs_tools()` returning `Vec<Arc<dyn Tool>>` and
    `register()` to plug into inventory.
  - `read_file.rs` — `ReadFileTool { default_offset: u32, default_limit: u32, max_chars: usize }`.
    - Inputs: `path: String`, `offset: Option<u32>`, `limit: Option<u32>`.
    - Resolves the path; if `path` contains `..` or is absolute and
      outside `working_dir` without explicit override, refuse (per
      `path_security`).
    - Returns a JSON object: `{content, total_lines, byte_size, truncated}`.
    - Honors `max_result_size_chars` from PR-1; emits a `truncated: true`
      marker if the cap clipped the result, with a hint to re-call with
      `offset`.
  - `write_file.rs` — `WriteFileTool`.
    - Inputs: `path: String`, `content: String`, `mode: Option<"create"|"overwrite"|"append">`.
    - Refuses if path matches `path_security::write_denied(path)`.
    - Returns `{bytes_written, sha256, mode}`.
  - `apply_patch.rs` — `ApplyPatchTool`.
    - Input: `patch: String` in V4A format
      (`*** Begin Patch` / `*** Update File` / `*** Add File` /
      `*** Delete File` / `*** Move File` / `*** End Patch`).
    - Use `patch_parser.rs` modeled on hermes-agent's parser; the
      per-operation application goes through the `FileOperations`
      abstraction (see PR-3).
    - Returns a per-op result list
      (`{op, path, status: "applied"|"rejected", reason?}`).
  - `search.rs` — `SearchTool`.
    - Inputs: `pattern: String`, `path: Option<String>` (default
      working_dir), `target: Option<"content"|"files"|"both">` (default
      `content`), `glob: Option<String>`, `max_results: Option<usize>`.
    - Backend: `grep`-via-`Command` initially, with a hook to swap in
      ripgrep when available. Use `tokio::process::Command` to keep
      async.
    - Returns `{matches: [{path, line, column, snippet}], total,
      truncated}`.
- `crates/hermes-agent/src/path_security.rs` (new) — port hermes-agent's
  write/read deny-list logic:
  - `write_denied_paths()` returning the deny list built from
    `home` + known credential paths.
  - `is_blocked_device(path)` (hermes-agent's `_BLOCKED_DEVICE_PATHS`).
  - `check_cross_profile(path, current_profile)` (hermes-agent's
    `_check_cross_profile_path`).
  - Unit tests for each.

**Acceptance:**

- Unit tests for `path_security::*` covering `~/.ssh/`, `/etc/shadow`,
  `/dev/zero`, `~/.hermes-hermes/skills/` cross-profile writes.
- Unit tests for `ApplyPatchTool`: parse, apply add/update/delete/move,
  reject malformed patch, reject patch referencing a denied path.
- Unit test: `ReadFileTool` honors `max_result_size_chars` and
  `truncated: true` flag.
- Unit test: `SearchTool` returns matches with `line`/`column` and
  honors `glob`.

**Why second:** these are the tools that will *most* reduce "agent
process does shell things" — and they're the lowest-risk layer-1
additions.

### PR-3: `FileOperations` backend abstraction (G12, G1/G2/G3/G4 internal)

Goal: prepare the codebase for non-local terminals (docker, ssh, remote
sandbox) so perry_hermes can grow the "runs anywhere" story without
touching every tool.

**Files to add/change:**

- `crates/hermes-agent/src/tools/fs/backend/` (new):
  - `mod.rs` — `pub trait FileOperations: Send + Sync` with
    `read_file`, `read_file_raw`, `write_file`, `apply_patch`,
    `search`. ABC like hermes-agent's `FileOperations`.
  - `local.rs` — `LocalFileOps` impl using
    `tokio::fs` / `tokio::process::Command` (for search).
  - `mod.rs` re-exports — `pub type SharedFileOps = Arc<dyn FileOperations>`.
- Refactor `ReadFileTool` / `WriteFileTool` / `ApplyPatchTool` /
  `SearchTool` to take a `SharedFileOps` in their constructor; default
  to `LocalFileOps::new(working_dir)`. This is an internal refactor —
  the LLM-facing tool schema and `Tool` trait stay the same.
- `crates/hermes-agent/src/config.rs` — add
  `agent.files.backend: Option<"local"|"docker"|"ssh">` config field,
  default `local`. v1 only implements `local`; the enum is reserved.

**Acceptance:**

- All PR-2 tests pass unchanged after the refactor (the refactor is
  supposed to be transparent).
- New test: `LocalFileOps_apply_patch_round_trips` — write a temp
  file, apply a V4A patch, read back, assert content matches expected.
- New test: `ReadFileTool_with_custom_backend` — feed a mock
  `FileOperations` returning canned content, assert the tool
  propagates the result.

**Why third:** PR-2 is the user-visible win; PR-3 makes the codebase
ready for sandboxing without requiring it today. Defer docker/ssh
impls to a follow-up; the trait is the contract.

### PR-4: `delegate_task`, `vision_analyze`, `session_search`, `todo` (G5, G6, G7, G8)

Goal: the high-leverage tools that turn perry_hermes from "single agent
with files" into "composable agent runtime".

Each is a non-trivial PR on its own. Decompose as follows:

- **PR-4a: `todo` (G8).** In-memory `TodoStore` per `AgentSession`;
  `TodoTool` reads/writes it. Re-inject the list into the next
  provider call after context compression. Hermes-agent's
  `tools/todo_tool.py` is a clean reference; the Rust version goes in
  `crates/hermes-agent/src/tools/todo.rs`.
- **PR-4b: `vision_analyze` (G5).** `crates/hermes-agent/src/tools/vision.rs`
  — takes `image_url: String` (or base64), `prompt: String`, and
  routes through a configurable vision model (the same provider layer
  the agent already uses, or a separate "auxiliary" provider). Honors
  `requires_env` for the API key. Most of the value is plumbing;
  hermes-agent's `tools/vision_tools.py` shows the routing pattern.
- **PR-4c: `session_search` (G7).** Requires an `AgentSession` SQLite
  store (FTS5) which perry_hermes doesn't have today — its persistence
  is `JsonFileSessionStore`. This PR is a **prerequisite for a
  cross-session recall feature**; if the team decides cross-session
  recall is out of scope, drop G7. If in scope, do a SQLite/FTS5 store
  first, then port hermes-agent's `session_search_tool.py` single-
  shape-with-three-modes design.
- **PR-4d: `delegate_task` (G6).** The biggest of the four. The
  design is:
  - `crates/hermes-agent/src/agent/subagent.rs` — `SubagentSpec {
    goal, context, toolset_allow, max_iterations, depth }`.
  - `SubagentHandle` — spawned via `tokio::spawn`; returns a
    `JoinHandle<SubagentOutcome>`.
  - The `DelegateTaskTool` blocks (with a `CancellationToken`) until
    the child completes, then returns the summary as a string to the
    LLM.
  - The child uses the same `AIAgent::run_session_turn` API the parent
    uses, with a restricted `InMemoryRegistry` and a fresh
    `AgentSession`. **No** new runtime code — just orchestration.
  - Depth cap (default 2) and concurrency cap (default 4) enforced
    inside the tool, not in the runtime.
  - Mirror hermes-agent's "parent never sees child intermediates;
    only the summary" contract.

**Acceptance (per sub-PR):**

- `todo`: round-trip write/read/merge; status transitions; survives
  `replace_messages` (it lives in a separate field, like
  `context_usage_baseline_tokens`).
- `vision_analyze`: with a mocked vision provider, return canned
  analysis; assert `requires_env` is honored.
- `session_search`: FTS5 round-trip; all three modes (discovery /
  scroll / browse) covered by separate tests.
- `delegate_task`: parent sees only summary; depth cap blocks depth-3
  children; concurrency cap blocks the 5th concurrent child; child
  errors propagate; child cancellation propagates; child's
  `AgentSession` is not visible to the parent.

**Why fourth:** these are the highest-leverage but also the most
expensive. Doing them last means PR-1/2/3 are already in, so the
`DelegateTaskTool` can be tested with the file-system tools available
to children.

## 4. The subagent-driven-development prerequisite (G6, callout)

The `subagent-driven-development` skill assumes the agent can dispatch
fresh subagents. That requires G6 (`delegate_task`) to land before
that skill is actually useful. This is worth flagging in the skill
description and in `AGENTS.md` so future contributors don't try to
invoke a `delegate_task`-using flow on a perry_hermes build that
doesn't have PR-4d yet.

When PR-4d lands, the skill description should be updated to point
concretely at `DelegateTaskTool` as the dispatch primitive, and the
"is it available?" check should be a registry query, not a "we hope
so" comment.

## 5. What this document is NOT

- **Not a plan to change the agent-session meta-tools** (the
  `terminal` / `read_file` / `write_file` / `skill_view` / `skills_list`
  the agent process sees in this session). Those are determined by
  whatever perry_hermes host runs the agent. If they need work, that
  goes in a separate document — possibly a "host runtime API" spec.
- **Not a plan to add 86 tools.** The 86 files in `tools/*.py` are
  LLM-callable *and* include ones that don't make sense for perry_hermes
  (browser, computer use, kanban, etc.). PR-1/2/3/4 cover the
  foundational ones. Adding domain-specific tools (browser, image
  gen, etc.) is its own future work.
- **Not a migration.** perry_hermes's existing `Tool` trait users
  (currently `terminal` and a few others) are preserved by PR-1's
  default-impl pattern. Nothing existing breaks.
- **Not a promise of feature parity.** hermes-agent has years of
  edge-case handling (path safety, container mirroring, cross-
  profile guards, scheduler, etc.). This document picks the gaps
  most likely to *improve the agent's day-to-day* and leaves the
  rest for follow-up.

## 6. Open questions for the implementer

1. **`inventory` crate vs hand-rolled discovery.** `inventory` requires
   a nightly Cargo feature flag (`[features] inventory = ["dep:inventory"]`)
   on stable Rust. The current perry_hermes MSRV is 1.75. Confirm
   `inventory` works on stable at MSRV 1.75, or write a small
   `build.rs` that scans the source tree for `register!(...)` macros
   and emits an inventory-list `mod`.
2. **V4A patch format strictness.** hermes-agent's parser is
   permissive in some places (trailing whitespace, blank line
   tolerance). Decide up front: do we accept the same permissiveness
   (more LLM-friendly) or do we require strict V4A (more predictable)?
3. **`todo` survival across `replace_messages`.** The natural design
   puts `TodoStore` in `AgentSession` (alongside
   `context_usage_baseline_tokens`). The alternative is to keep it on
   the `AIAgent` (alongside the loop). The former is hermes-agent's
   choice; we should follow it for "session-scoped" semantics.
4. **Where does `vision_analyze` route?** Two options: through the
   same provider used for chat (slightly awkward — wrong message
   shape, no streaming), or through a dedicated "auxiliary" provider
   declared in config. The dedicated path is cleaner; the shared
   path is cheaper to ship.
5. **`session_search` vs `JsonFileSessionStore`.** The current store
   is JSON-per-session. Adding FTS5 means adding SQLite. Two
   approaches: (a) replace JSON with SQLite for the whole session
   store (big change, touches the agent loop's persistence contract),
   or (b) keep JSON as the source of truth, build a derived SQLite
   index on startup (smaller blast radius, slower write path).
   Approach (b) is the conservative one.

## 7. Acceptance for the whole document

- [ ] All four PRs merged; `cargo test --workspace`,
      `cargo clippy --all-targets --all-features -- -D warnings`, and
      `cargo doc --no-deps` all green at each merge.
- [ ] `AGENTS.md` "Architecture" table grows a row for each new tool
      module.
- [ ] Each new tool has a doc comment + an integration test
      (use a `ScriptedProvider` pattern where the LLM-facing behavior
      matters; use plain unit tests for the backend).
- [ ] `subagent-driven-development` skill description points at
      `DelegateTaskTool` as the dispatch primitive (after PR-4d).
- [ ] The "user-visible symptom" column in §2 is re-checked in a
      follow-up session and either confirmed fixed or annotated with
      "still symptomatic because X".
