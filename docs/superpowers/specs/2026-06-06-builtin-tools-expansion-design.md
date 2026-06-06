# Builtin Tools Expansion — `read_file` / `write_file` / `skills_list` / `skill_view`

**Date:** 2026-06-06
**Status:** Proposed
**Supersedes:** the "SkillActivationTool is a Phase 12 deliverable" wording in `2026-06-05-phase-9-skills-loading-design.md` (we deliver four tools, not one) and the placeholders in `hermes-skills` `render_system_prompt_block` that reference a `skill_view` tool that did not yet exist
**Reference implementation:** `/Users/amagicpear/.hermes/hermes-agent/tools/{file_tools.py, file_operations.py, skills_tool.py}` (Python source of truth for naming, toolset categorization, and observed behavior)

## 1. Goal

Add four built-in tools to `hermes-agent` so the LLM can read and write local files and consult skill bodies on demand, matching the public surface of the Python reference. This unblocks the system-prompt skill index (already shipped in Phase 9) and brings the runtime's tool catalog from one tool to five.

| Tool | Purpose |
|---|---|
| `read_file` | Read a text file with line numbers, pagination, and binary/device guards |
| `write_file` | Atomically write a text file, creating parent directories, preserving line endings and BOM |
| `skills_list` | List installed skills (name + description + category) |
| `skill_view` | Load a skill's `SKILL.md` body or one of its linked files |

## 2. Non-Goals

- Skill creation, editing, deletion (`skill_manage`) — P2
- Skill marketplace / hub / install — P2
- File `patch` / `search_files` — P2 (the four we ship are read+write for files, list+view for skills)
- `ToolContext.permissions` extension (no new permission flag) — already noted in CLAUDE.md
- Sensitive-path deny list (`/etc/`, `~/.hermes/auth.json`, etc.) — P1 (matches CLAUDE.md "ToolContext.permissions is not enforced")
- Cross-task read dedup + repeat-read blocking — P1 (Python has it; we defer to keep this spec tight)
- LSP-driven syntax/lint feedback on write — N/A in Rust runtime
- Plugin / `namespace:skill` dispatch — P2 (no plugin system on the Rust side)
- Per-tool config knobs (Python has `file_read_max_chars` etc.) — P1

## 3. Naming and Toolset Mapping

Naming and toolset categorization are copied verbatim from the Python registry. The `disabled_toolsets` filter in `[agent]` already works on toolset names; mapping them out from day one means existing config and docs transfer cleanly.

| Rust type | `name()` | `toolset()` | Python `name` | Python `toolset` |
|---|---|---|---|---|
| `ReadFileTool` | `read_file` | `"file"` | `read_file` | `file` |
| `WriteFileTool` | `write_file` | `"file"` | `write_file` | `file` |
| `SkillListTool` | `skills_list` | `"skills"` | `skills_list` | `skills` |
| `SkillViewTool` | `skill_view` | `"skills"` | `skill_view` | `skills` |

`BashTool` keeps `name: "bash"`, `toolset: "core"` (untouched). Adding to `core` is reserved for tools that the agent cannot meaningfully operate without; the four here are user-disablable in the Python source, and we follow that lead.

## 4. Architecture

### 4.1 File layout

```
crates/hermes-agent/src/tools/
├── mod.rs            # pub mod bash; pub mod files; pub mod skills;
├── bash.rs           # unchanged
├── files.rs          # new: ReadFileTool, WriteFileTool, shared helpers
└── skills.rs         # new: SkillListTool, SkillViewTool

crates/hermes-agent/src/tool_catalog.rs
                      # build_registry(disabled, skills_dir) wires all four
                      # tools + BashTool
```

Two files instead of one because file and skill are two unrelated concerns; tests group cleanly. We do not touch `bash.rs` and do not propose unrelated refactors.

### 4.2 Dependency direction

```
tools/files.rs   → hermes-core        (Tool trait, ToolError, ToolContext, ToolOutput)
tools/skills.rs  → hermes-core, hermes-skills
tool_catalog.rs  → tools/*, hermes-core
```

No new crate. No new edge in the workspace graph. `hermes-skills` already exposes `load_skills(&Path) -> Vec<Skill>` and `Skill { name, qualified_name, category, description, source_path, frontmatter }` — the new tools consume both.

### 4.3 `tool_catalog::build_registry` signature change

```rust
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry
```

- `AIAgent::from_config(HermesConfig)` resolves the skills directory using the same rules `hermes_skills::load_skills` uses (`HERMES_HOME` env → `~/.perry_hermes` → ensure the directory exists, create it if missing), then passes the `PathBuf` in. The `skills_dir` is **read-only at tool-call time** — write/edit skill bodies is a P2 `skill_manage` deliverable, not in scope.
- The existing call site in `tool_catalog.rs`'s own `#[cfg(test)]` (and any other test that builds a registry directly) gains a `&Path` argument — a one-line update in each. The default test path can be `Path::new("/tmp/hermes-test-skills")`; behavior of the existing assertions is unchanged.

### 4.4 `lib.rs` re-exports

```rust
pub use tools::{
    bash::BashTool,
    files::{ReadFileTool, WriteFileTool},
    skills::{SkillListTool, SkillViewTool},
};
```

## 5. Tool Specifications

### 5.1 `ReadFileTool`

**Parameters (JSON Schema, draft-07):**

```json
{
  "type": "object",
  "properties": {
    "path":     { "type": "string",  "description": "Path to the file (absolute, relative to working_dir, or ~/path)" },
    "offset":   { "type": "integer", "default": 1, "minimum": 1, "description": "1-indexed line number to start from" },
    "limit":    { "type": "integer", "default": 500, "minimum": 1, "maximum": 2000, "description": "Maximum lines to return" }
  },
  "required": ["path"],
  "additionalProperties": false
}
```

**Description:** *"Read a text file with line numbers and pagination. Use this instead of cat/head/tail in bash. Output format: 'LINENO|CONTENT'. Suggests similar filenames if not found. Use offset and limit for large files. Reads exceeding ~100K characters are rejected; use offset and limit to read specific sections. Cannot read images or binary files."*

**Behavior:**

1. Blocklist check (pure path check, no I/O): compare the literal (un-resolved) input path against the blocklist, then `std::fs::canonicalize` the path and compare again. Blocklist covers `/dev/{zero,urandom,random,full,stdin,tty,console,stdout,stderr}`, `/dev/fd/{0,1,2}`, `/proc/self/fd/{0,1,2}`, and `/proc/<pid>/{environ,cmdline,maps}`. Either check matching short-circuits to the error.
2. Binary-extension check (no I/O): reject if `path.extension()` is in the same list `tools/binary_extensions.py` uses — `.png`, `.jpg`, `.gif`, `.pdf`, `.zip`, `.tar`, `.gz`, `.exe`, `.dll`, `.so`, `.dylib`, `.class`, `.pyc`, `.wasm`, `.mp4`, `.mp3`, `.wav`, `.flac`, `.ogg`, `.webp`, `.heic`, `.avif`, `.ico`, `.ttf`, `.otf`, `.woff`, `.woff2`, `.eot`, `.psd`, `.ai`, `.sketch`, `.fig`, `.blend`, `.glb`, `.gltf`, `.obj`, `.fbx`, `.stl`, `.3ds`, `.dae`, `.svg`, `.db`, `.sqlite`, `.sqlite3`, `.bin`, `.dat`, `.iso`, `.dmg`, `.deb`, `.rpm`. Returns error.
3. Resolve `~` and relative paths against `ctx.working_dir`.
4. Read first 1000 bytes; if `>5%` non-printable, treat as binary → error.
5. Read the requested range (offset 1-indexed, inclusive of `offset+limit-1`) and number each line: format `{LINENO:>W}|{CONTENT}\n` where `W = (offset + limit).to_string().len()`. `offset==1` strips a leading UTF-8 BOM (U+FEFF) before numbering.
6. Preserve line endings as found on disk. Python normalizes; we don't, because the LLM only sees the content as a string and the byte count is a separate signal. (Documented divergence, P1 candidate to align.)
7. Character-level cap: if `content.len() > 100_000`, truncate head+tail with an omission notice using the same head/tail shape as `BashTool::truncate_output`. To avoid duplicating the helper, the truncation function in `tools/bash.rs` is moved to a `tools/util.rs` module (or `pub(crate)` re-exported); see §9.
8. Truncation / `total_lines > offset+limit-1` → set `truncated: true` and add `hint: "Use offset=<N+1> to continue reading (showing A-B of TOTAL lines)"`.
9. File not found → run a `similar_files` search in the parent dir: score candidates by (exact > same stem/diff ext > prefix > substring > same-ext char-overlap ≥ 40 %), take top 5, include in error response.

**Return shape (success):**

```json
{
  "content": "    1|fn main() {\n    2|    println!(\"hi\");\n    3|}\n",
  "total_lines": 3,
  "file_size": 42,
  "truncated": false,
  "hint": null
}
```

**Return shape (error — wrapped in `ToolOutput`, not raised):**

```json
{ "error": "File not found: /tmp/missing.rs", "similar_files": ["/tmp/missing.py", "..."] }
```

### 5.2 `WriteFileTool`

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "path":    { "type": "string",  "description": "Path to write (created if missing, overwritten if exists)" },
    "content": { "type": "string",  "description": "Complete file content" },
    "mode":    { "type": "string",  "enum": ["overwrite", "append"], "default": "overwrite" }
  },
  "required": ["path", "content"],
  "additionalProperties": false
}
```

`mode: append` is the only behavior this spec adds beyond Python (Python only has overwrite; common shell semantics treats `>>` as append, and `file_write_tool` doesn't expose it — but it's cheap and the LLM has asked for it before). If reviewers want to drop it for strict parity, it's a one-line schema removal.

**Description:** *"Write content to a file, completely replacing (or appending to) existing content. Creates parent directories automatically. Preserves the original file's line endings (CRLF stays CRLF) and BOM. Atomic: writes to a temp file in the same directory and renames."*

**Behavior:**

1. Resolve `~` and relative path.
2. Probe the existing file (if any): read up to 100 KB to detect BOM and dominant line ending (`\r\n` vs `\n`).
3. If existing file is CRLF → normalize `content` to CRLF. If existing file has a BOM and `content` does not → prepend BOM. Idempotent (round-trip preserves the byte signature).
4. `fs::create_dir_all(parent)` if missing → set `dirs_created: true`.
5. Write: create `path.with_extension(format!("{}.hermes-tmp.{}", path.extension().and_then(|s| s.to_str()).unwrap_or(""), std::process::id()))` in the same directory, write content, fsync, then `fs::rename` over `path`. On any failure, `fs::remove_file` the temp. POSIX rename within the same directory is atomic.
6. Resolve the final path via `canonicalize` and include in the return as `resolved_path` (lets the model see "I wrote to /abs/path, not the relative I asked for" — same affordance Python gives).

**Return shape:**

```json
{
  "bytes_written": 1234,
  "dirs_created": true,
  "resolved_path": "/abs/path/to/file.rs"
}
```

Errors: `{ "error": "Failed to write file: <os error>" }` wrapped in `ToolOutput`.

### 5.3 `SkillListTool`

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "category": { "type": "string", "description": "Optional case-insensitive category filter" }
  },
  "additionalProperties": false
}
```

**Description:** *"List available skills (name + description + category). Use skill_view(name) to read a skill's body."*

**Behavior:**

1. `hermes_skills::load_skills(&skills_dir)` — already does the frontmatter validation and exclusion list.
2. Filter by `category` if given (case-insensitive equality on `Skill.category`).
3. Sort: `(category, name)` ascending; skills with no category sort last.
4. Extract unique categories.
5. Return JSON: `{ success, count, categories, skills: [{ name, qualified_name, category, description }, ...], hint }`.
6. Empty / missing directory → `{ success: true, count: 0, skills: [], categories: [], message: "No skills found. Skills directory will be created at <path>." }` (matches Python — not an error).

**Errors:** invalid frontmatter propagates `ToolError::Execution` (loop wraps it in `role: tool` so the model sees it; we deliberately don't soften this because it's a config error).

### 5.4 `SkillViewTool`

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "name":      { "type": "string", "description": "Skill name (use skills_list to see available skills)" },
    "file_path": { "type": "string", "description": "Optional: path to a linked file within the skill (e.g. 'references/api.md')" }
  },
  "required": ["name"],
  "additionalProperties": false
}
```

**Description:** *"Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files. First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path."*

**Behavior:**

1. Reject names containing `:` (plugin namespace — P2, not supported in Rust runtime yet). Error: `"Plugin skills not supported; name should not contain ':'"`.
2. `hermes_skills::load_skills(&skills_dir)`, find by `name` (case-sensitive on `Skill.name`, which is `frontmatter.name`).
3. If not found → `{ success: false, error: "Skill '<name>' not found", available: [name, name, ...] }`.
4. If `file_path`:
   - Compute `candidate = skill.source_path.parent().join(file_path)`.
   - Reject if the joined path contains `..` segments OR if `canonicalize`d `candidate` is not under `canonicalize`d `skill.source_path.parent()` → `InvalidArgs`.
   - Reject if `candidate` is not a regular file → `InvalidArgs`.
   - Read raw bytes, UTF-8 validate, return `{ success, name, file_path, content, description }`.
5. If no `file_path`:
   - Read `skill.source_path` content.
   - Strip YAML frontmatter (use `hermes_skills::frontmatter` parser to find the `---\n...\n---\n` block, return the rest).
   - Char-truncate at 100K (head+tail with notice).
   - Discover linked files: scan `references/`, `templates/`, `scripts/` subdirs of the skill dir, collect filenames. Return as `{references: [...], templates: [...], scripts: [...]}` (or empty list per missing dir; never null).
6. `readiness_status: "available"` always (we don't model setup_needed / unsupported in this spec).

**Return shape (no file_path, success):**

```json
{
  "success": true,
  "name": "axolotl",
  "content": "# Axolotl ...\n...",
  "description": "Fine-tuning framework for LLMs",
  "linked_files": { "references": ["dataset-formats.md"], "templates": ["config.yaml"], "scripts": [] },
  "readiness_status": "available"
}
```

**Return shape (file_path, success):**

```json
{
  "success": true,
  "name": "axolotl",
  "file_path": "references/dataset-formats.md",
  "content": "# Dataset formats ...",
  "description": "Fine-tuning framework for LLMs"
}
```

## 6. Registration

```rust
// hermes-agent/src/tool_catalog.rs
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry {
    let mut reg = InMemoryRegistry::new();

    if !disabled_toolsets.iter().any(|s| s == "core" || s == "terminal") {
        reg = reg.register(Arc::new(BashTool::new()));
    }
    if !disabled_toolsets.iter().any(|s| s == "file") {
        reg = reg
            .register(Arc::new(ReadFileTool::new(skills_dir.to_path_buf())))
            .register(Arc::new(WriteFileTool::new()));
    }
    if !disabled_toolsets.iter().any(|s| s == "skills") {
        reg = reg
            .register(Arc::new(SkillListTool::new(skills_dir.to_path_buf())))
            .register(Arc::new(SkillViewTool::new(skills_dir.to_path_buf())));
    }
    reg
}
```

`AIAgent::from_config` resolves `skills_dir` using the same rules `hermes_skills` already uses (`HERMES_HOME` env var, falling back to `~/.perry_hermes`, falling back to "create the directory if missing"). Tools hold the path as a constructor arg so tests can inject a `tempfile::TempDir`-backed path without env mutation.

The existing test in `tool_catalog.rs` calling `build_registry(&["terminal"])` becomes `build_registry(&["terminal"], &PathBuf::from("/tmp/fake-skills"))` — a one-line update, covered by the new tests in §7.

## 7. Testing

TDD: every tool gets a failing test first (`cargo test` red), then minimal impl (green), then refactor.

| Test file | Coverage |
|---|---|
| `crates/hermes-agent/tests/files.rs` (new) | read: defaults, offset+limit pagination, line-number formatting, device-path block (literal + canonicalized), binary-extension block, BOM strip, file-not-found + similar suggestions, char truncation, CRLF passthrough, relative path resolution. write: new file + parent mkdir, overwrite, append mode, CRLF preservation, BOM preservation, atomic rename (verified by inspecting temp file behavior in a test that simulates mid-write failure by injecting an unwritable temp parent), resolve_path in return. |
| `crates/hermes-agent/tests/skills.rs` (new) | list: empty dir, multiple skills, `category` filter case-insensitive, `(category, name)` sort, frontmatter invalid → `ToolError::Execution`. view: found (no file_path), found (with file_path), frontmatter stripped, not-found with `available` list, `..` traversal rejected, `:` in name rejected, char truncation. |
| `crates/hermes-agent/tests/tool_dispatch.rs` (extend) | `disabled_toolsets=["file"]` excludes read_file + write_file; `["skills"]` excludes skills_list + skill_view; `["core"]` excludes bash only. |
| `crates/hermes-agent/tests/skills_injection.rs` (extend) | E2E: `ScriptedProvider` returns an assistant `tool_call` for `skill_view`; loop routes to `SkillViewTool`; `role: tool` message carries the JSON; next provider turn sees it. |

Tests use `tempfile::TempDir` for working directories and skills dirs, plus a shared `assertions` helper for "line-number prefix" formatting.

## 8. Error Handling

| Scenario | Outcome | Type |
|---|---|---|
| JSON Schema fail (missing `path`, `limit > 2000`) | Loop rejects before dispatch | existing `jsonschema` path |
| File not found | `{"error", "similar_files": [...]}` wrapped in `ToolOutput` | non-fatal |
| Path is a directory | `{"error": "<path> is a directory"}` | non-fatal |
| Binary extension | `{"error": "Cannot read binary file '<path>' (.<ext>). Use vision_analyze for images."}` | non-fatal |
| Device path | `{"error": "Cannot read '<path>': device file would block or produce infinite output."}` | non-fatal |
| Non-UTF-8 content | `{"error": "File is not valid UTF-8 text"}` | non-fatal |
| Write: parent not creatable / EROFS / EACCES | `{"error": "Failed to write file: <os error>"}` | non-fatal |
| Write: rename failed | cleaned-up temp + `{"error": "Atomic write failed: ..."}` | non-fatal |
| Skill name not found | `{"success": false, "error", "available": [...]}` | non-fatal |
| Skill frontmatter invalid (list path) | `ToolError::Execution` (loop wraps in `role: tool`) | non-fatal but logged |
| Skill `file_path` `..` traversal | `{"error": "file_path escapes skill directory: <resolved>"}` | non-fatal |
| Skill name with `:` | `{"error": "Plugin skills not supported; ..."}` | non-fatal |
| Cancel / timeout mid-read | `tokio::select!` on cancel + sleep; partial bytes discarded | `ToolError::Cancelled` |
| Cancel / timeout mid-write | Drop the in-progress temp file (best-effort) | `ToolError::Cancelled` |

**Convention:** anything caused by user-supplied input (path, content, skill name) is a `ToolOutput` error so the model can react. Anything that means the tool itself is broken (frontmatter parse, OS report) is a `ToolError::Execution` so the loop logs it but the conversation continues.

## 9. Impact on Existing Code

| File | Change |
|---|---|
| `crates/hermes-agent/src/tools/mod.rs` | Add `pub mod files; pub mod skills; pub mod util;` |
| `crates/hermes-agent/src/tools/bash.rs` | Replace local `truncate_output` with `crate::tools::util::truncate_output` (or `pub(crate) use`) — pure move, no behavior change |
| `crates/hermes-agent/src/tools/util.rs` | New: `truncate_output(&str, max_chars) -> String` (lifted from bash.rs) |
| `crates/hermes-agent/src/tools/{files,skills}.rs` | New |
| `crates/hermes-agent/src/tool_catalog.rs` | `build_registry(&[String], &Path)`; new wiring; existing in-file `#[cfg(test)]` gains the `&Path` arg |
| `crates/hermes-agent/src/runtime_agent.rs` | Resolve `skills_dir` and pass to `build_registry`; resolve using `HERMES_HOME` → `~/.perry_hermes` rules |
| `crates/hermes-agent/src/lib.rs` | Re-export `ReadFileTool` etc. |
| `crates/hermes-agent/tests/{files,skills}.rs` | New |
| `crates/hermes-agent/tests/tool_dispatch.rs` | Existing call sites gain the `&Path` arg; add the disabled-toolset regression tests |
| `crates/hermes-agent/tests/skills_injection.rs` | Add the E2E loop test |
| `hermes-skills` | No change (the spec uses existing `load_skills` + `Skill`) |
| `hermes-core` | No change |
| `hermes-providers` | No change |
| `hermes-cli` | No change |
| `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md` | Strike the "SkillActivationTool is a Phase 12 deliverable" line; the system-prompt block now actually points to a real tool |

## 10. P1 / P2 Backlog (Out of Scope, Listed for Visibility)

- `ToolPermissions` extension: `pub write_file: bool` so a gateway can deny writes (current `subprocess` flag pattern)
- Sensitive-path deny list (`/etc/`, `~/.hermes/auth.json`, Docker socket, etc.) — needs `write_denied` table from `agent.file_safety` ported
- Cross-task read dedup + repeat-read hard block (Python's `_read_tracker`)
- Per-tool config (`[agent].file_read_max_chars`, `[agent].file_write_atomic`, etc.)
- Lint-delta / syntax error feedback on write (Python uses in-process `ast.parse` / `json.loads`; we could do the same for `#[cfg(feature = "lint")]`)
- `patch` and `search_files` tools (Python's other `file` toolset members)
- `skill_manage` (create / edit / delete skills; needs `write_file` to land first for `SKILL.md` editing, then this becomes a thin layer above it)
- Plugin / `namespace:skill` dispatch
- BOM detection threshold tuning and image MIME sniffing
- Image / audio / video read paths (vision / transcription)

## 11. Open Decisions Resolved During Brainstorming

- **Scope:** four tools (read_file, write_file, skills_list, skill_view). patch / search_files / skill_manage are P2.
- **Permissions:** no new `ToolContext.permissions` flags. Errors are reported as `ToolOutput` so the model self-corrects.
- **Skills source:** `~/.perry_hermes/skills/` only, resolved the same way `hermes_skills::load_skills` already does it.
- **File layout:** two new files (`files.rs`, `skills.rs`). `BashTool` untouched.
- **Naming / toolset:** copy Python verbatim (`read_file` / `write_file` in `"file"`, `skills_list` / `skill_view` in `"skills"`).
- **`mode: "append"`:** included as the one Rust-side behavioral extension to `write_file`. Trivial to drop if reviewers want strict parity.
- **Line-ending normalization on read:** we don't (Python does). Documented divergence; revisit if it bites.
- **Cross-task read dedup:** deferred to P1. Worth adding when the gateway ships; not in this spec.
