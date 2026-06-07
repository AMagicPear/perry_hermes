# Builtin Tools Expansion — `terminal` / `read_file` / `write_file` / `skills_list` / `skill_view`

**Date:** 2026-06-06
**Status:** Proposed
**Supersedes:** the "SkillActivationTool is a Phase 12 deliverable" wording in `2026-06-05-phase-9-skills-loading-design.md` (we deliver four new built-in tools now and keep the existing terminal tool aligned with Python naming) and the placeholders in `hermes-skill-loader` `render_system_prompt_block` that reference a `skill_view` tool that did not yet exist
**Reference implementation:** `/Users/amagicpear/.hermes/hermes-agent/tools/{terminal_tool.py,file_tools.py,file_operations.py,skills_tool.py}` (Python source of truth for naming, schema, toolset categorization, and externally visible behavior)

## 1. Goal

Expand `hermes-agent`'s built-in tool catalog so the Rust runtime exposes the same public tool surface as the Python runtime for terminal execution, file reads/writes, and skill discovery/loading.

This phase adds four new built-in tools to the Rust runtime:

| Tool | Purpose |
|---|---|
| `read_file` | Read a text file with line numbers and pagination |
| `write_file` | Replace a file's full contents, creating parent directories as needed |
| `skills_list` | List available skills with minimal metadata |
| `skill_view` | Load a skill body or one linked file inside a skill |

The runtime tool catalog becomes five built-in tools total once these land:

| Tool | Status |
|---|---|
| `terminal` | Existing; rename/alignment work in this spec |
| `read_file` | New |
| `write_file` | New |
| `skills_list` | New |
| `skill_view` | New |

## 2. Non-Goals

- File `patch` / `search_files` — P2
- Skill creation, editing, deletion (`skill_manage`) — P2
- Skill marketplace / hub / install flows — P2
- Prompt preprocessing / shell-template expansion inside `skill_view` — P2
- Plugin skill loading (`plugin:skill`) — P2 on the Rust runtime even though Python supports it today
- External skills dirs beyond the resolved local skills root — P2
- Full Python parity for read dedup / repeat-read blocking / stale-read tracking / cross-agent file coordination — P1/P2 follow-up
- Full Python parity for syntax/lint feedback after `write_file` — P1/P2 follow-up
- `ToolContext.permissions` expansion for file writes — out of scope here
- Sensitive-path deny list parity with Python's full file safety stack — P1
- Skill environment-readiness capture (`required_environment_variables`, secret collection flows) — P2

## 3. Naming and Toolset Mapping

The public tool names, toolsets, and schema descriptions should match the Python registry for all tools covered by this phase.

| Rust type | `name()` | `toolset()` | Python `name` | Python `toolset` |
|---|---|---|---|---|
| `TerminalTool` | `terminal` | `"terminal"` | `terminal` | `terminal` |
| `ReadFileTool` | `read_file` | `"file"` | `read_file` | `file` |
| `WriteFileTool` | `write_file` | `"file"` | `write_file` | `file` |
| `SkillListTool` | `skills_list` | `"skills"` | `skills_list` | `skills` |
| `SkillViewTool` | `skill_view` | `"skills"` | `skill_view` | `skills` |

This implies a rename/alignment of the current Rust `BashTool` public surface:

- `name()` changes from `bash` to `terminal`
- `toolset()` changes from `core` to `terminal`
- `description()` and parameters schema align with Python's `TERMINAL_SCHEMA`
- `tool_catalog` filtering drops the current `core` special case for the shell tool and instead keys off `terminal`

Internal Rust type names do not need to match Python exactly; keeping the struct named `BashTool` is acceptable if we want a smaller diff, but the externally visible tool contract must be `terminal`.

## 4. Architecture

### 4.1 File layout

```text
crates/hermes-agent/src/tools/
├── mod.rs            # pub mod bash; pub mod files; pub mod skills;
├── bash.rs           # existing shell tool implementation, aligned to public `terminal` contract
├── files.rs          # new: ReadFileTool, WriteFileTool, shared helpers
└── skills.rs         # new: SkillListTool, SkillViewTool

crates/hermes-agent/src/tool_catalog.rs
                      # build_registry(disabled, skills_dir) wires terminal + four new tools
```

We keep the shell implementation in `bash.rs` for now to avoid churn, but from the model's perspective the tool is `terminal`.

### 4.2 Dependency direction

```text
tools/bash.rs    → hermes-core
tools/files.rs   → hermes-core
tools/skills.rs  → hermes-core, hermes-skill-loader
tool_catalog.rs  → tools/*, hermes-core
```

No new crate.

### 4.3 Shared skills-dir resolution

Introduce a single Rust-side helper used by both prompt composition and tool registration:

```rust
pub fn resolve_skills_dir() -> anyhow::Result<PathBuf>
```

Resolution rules for this phase:

1. `HERMES_HOME` when set
2. else `HOME/.perry_hermes`
3. append `/skills`
4. create the directory on first access when missing

`compose_system_prompt(...)` and `build_registry(...)` must both consume this shared resolver so the system-prompt skill index and runtime tools read the same directory.

### 4.4 `tool_catalog::build_registry` signature change

```rust
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry
```

`AIAgent::from_config(HermesConfig)` resolves `skills_dir` once and passes it down.

## 5. Tool Specifications

### 5.1 `TerminalTool`

The current Rust shell tool should align its public contract to Python's `terminal` tool.

**Parameters (must match Python surface):**

```json
{
  "type": "object",
  "properties": {
    "command": { "type": "string", "description": "The command to execute on the VM" },
    "background": { "type": "boolean", "default": false },
    "timeout": { "type": "integer", "minimum": 1 },
    "workdir": { "type": "string", "description": "Working directory for this command (absolute path). Defaults to the session working directory." },
    "pty": { "type": "boolean", "default": false },
    "notify_on_complete": { "type": "boolean", "default": false },
    "watch_patterns": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["command"]
}
```

**Description:** copy Python's `TERMINAL_TOOL_DESCRIPTION` verbatim into the Rust schema payload for this phase.

**Behavior note:** full backend parity with Python terminal environments is not in scope here; this spec only requires name / toolset / schema / user-facing description alignment so prompts and provider tool calls are consistent.

### 5.2 `ReadFileTool`

**Parameters:** copied from Python's `READ_FILE_SCHEMA`.

```json
{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "Path to the file to read (absolute, relative, or ~/path)" },
    "offset": { "type": "integer", "description": "Line number to start reading from (1-indexed, default: 1)", "default": 1, "minimum": 1 },
    "limit": { "type": "integer", "description": "Maximum number of lines to read (default: 500, max: 2000)", "default": 500, "maximum": 2000 }
  },
  "required": ["path"]
}
```

**Description:**

> Read a text file with line numbers and pagination. Use this instead of cat/head/tail in terminal. Output format: 'LINE_NUM|CONTENT'. Suggests similar filenames if not found. Use offset and limit for large files. Reads exceeding ~100K characters are rejected; use offset and limit to read specific sections of large files. NOTE: Cannot read images or binary files — use vision_analyze for images.

**Behavior:**

1. Resolve `~` and relative paths against `ctx.working_dir`.
2. Reject blocked device/fd/proc paths using the same path-only guard shape as Python.
3. Reject known binary extensions before reading.
4. Read and format the requested line range with numbered `LINE_NUM|CONTENT` prefixes.
5. Return JSON text in the same high-level shape Python uses: `content`, `total_lines`, `file_size`, `truncated`, plus optional `_hint` / `_warning` style guidance fields.
6. Enforce the ~100K character safety guard on returned formatted content.
7. When the file is unchanged and the exact same region is re-read within the same task, repeated-read dedup / blocking is a follow-up item, not required for this phase.

**Required parity:** tool name, schema, line-numbered output shape, blocked-device guard, binary-file rejection, character cap, and file-not-found suggestion behavior.

### 5.3 `WriteFileTool`

**Parameters:** copied from Python's `WRITE_FILE_SCHEMA`.

```json
{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "Path to the file to write (will be created if it doesn't exist, overwritten if it does)" },
    "content": { "type": "string", "description": "Complete content to write to the file" },
    "cross_profile": {
      "type": "boolean",
      "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Hermes profile's skills/plugins/cron/memories — by default these writes are blocked with a warning because they affect a different profile than the one this session is running under.",
      "default": false
    }
  },
  "required": ["path", "content"]
}
```

**Description:**

> Write content to a file, completely replacing existing content. Use this instead of echo/cat heredoc in terminal. Creates parent directories automatically. OVERWRITES the entire file — use 'patch' for targeted edits. Auto-runs syntax checks on .py/.json/.yaml/.toml and other linted languages; only NEW errors introduced by this write are surfaced (pre-existing errors are filtered out).

**Behavior:**

1. Resolve `~` and relative paths.
2. Reject sensitive-path writes and cross-profile writes unless `cross_profile=true`.
3. Reject attempts to write internal `read_file` status text back to disk.
4. Create parent directories automatically.
5. Replace the full file contents; no append mode in this phase.
6. Include `resolved_path` in the success payload.
7. Preserve the Python-compatible result shape from file operations as closely as practical.
8. Syntax/lint delta reporting is the intended public behavior, but if Rust cannot ship that in the same patch, the spec should call it out explicitly as a documented temporary divergence rather than silently dropping it.

### 5.4 `SkillListTool`

**Parameters:** copied from Python's `SKILLS_LIST_SCHEMA`.

```json
{
  "type": "object",
  "properties": {
    "category": {
      "type": "string",
      "description": "Optional category filter to narrow results"
    }
  },
  "required": []
}
```

**Description:**

> List available skills (name + description). Use skill_view(name) to load full content.

**Behavior:**

1. Ensure the local skills directory exists; create it if missing.
2. Scan local skills, returning minimal metadata only: `name`, `description`, `category`.
3. Filter by `category` when provided.
4. Sort by `(category, name)`.
5. Return `{ success, skills, categories, count, hint }` on success.
6. Invalid or unreadable individual skills should be skipped best-effort, matching current Python behavior.

### 5.5 `SkillViewTool`

**Parameters:** copied from Python's `SKILL_VIEW_SCHEMA`.

```json
{
  "type": "object",
  "properties": {
    "name": {
      "type": "string",
      "description": "The skill name (use skills_list to see available skills). For plugin-provided skills, use the qualified form 'plugin:skill' (e.g. 'superpowers:writing-plans')."
    },
    "file_path": {
      "type": "string",
      "description": "OPTIONAL: Path to a linked file within the skill (e.g., 'references/api.md', 'templates/config.yaml', 'scripts/validate.py'). Omit to get the main SKILL.md content."
    }
  },
  "required": ["name"]
}
```

**Description:**

> Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files (references, templates, scripts). First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path parameter.

**Behavior in this phase:**

1. Load from the resolved local skills dir only.
2. Support bare names and categorized local paths like `category/skill-name`.
3. If multiple local skills collide on the same bare name, return an ambiguity error instead of guessing.
4. If `file_path` is provided, reject traversal and require the target to remain under the skill directory after canonicalization.
5. On main-skill reads, return:
   - `success`
   - `name`
   - `content`
   - `description`
   - `linked_files`
   - `readiness_status`
6. `linked_files` should include `references`, `templates`, `assets`, `scripts`, and `other` buckets, matching Python's public response shape.
7. `readiness_status` should at least support `available` and `unsupported` in this phase.
8. Plugin-qualified names (`plugin:skill`) are intentionally unsupported in this phase even though the Python runtime supports them; return a clear unsupported error instead of silently misresolving them.
9. `required_environment_variables` and `missing_required_environment_variables` are desirable parity fields; include them if they are cheap to expose from Rust-side frontmatter parsing, otherwise list them as temporary documented divergence.

## 6. Registration

```rust
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry {
    let mut reg = InMemoryRegistry::new();

    if !disabled_toolsets.iter().any(|s| s == "terminal") {
        reg = reg.register(Arc::new(BashTool::new())); // public name() == "terminal"
    }
    if !disabled_toolsets.iter().any(|s| s == "file") {
        reg = reg
            .register(Arc::new(ReadFileTool::new()))
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

The runtime should stop treating `core` as the public shell-tool toolset for this path. If backward compatibility for existing config matters, we can temporarily accept both `core` and `terminal` in config parsing, but the registry should advertise `terminal`.

## 7. Testing

| Test file | Coverage |
|---|---|
| `crates/hermes-agent/tests/files.rs` | `read_file` schema/description parity, blocked device paths, binary rejection, numbered output, size cap, not-found suggestions, `write_file` overwrite behavior, parent-dir creation, `cross_profile` arg acceptance, `resolved_path` in return |
| `crates/hermes-agent/tests/skills.rs` | `skills_list` metadata shape, category filter, empty-dir creation path, `skill_view` main-body load, linked file load, traversal rejection, ambiguous-name rejection, unsupported plugin-qualified names |
| `crates/hermes-agent/tests/tool_dispatch.rs` | `disabled_toolsets=["terminal"]` excludes shell tool, `file` excludes read/write, `skills` excludes list/view |
| `crates/hermes-agent/tests/skills_injection.rs` | end-to-end `skill_view` tool call routing |
| `crates/hermes-agent/tests/terminal_schema.rs` or existing shell-tool tests | public `name() == "terminal"`, `toolset() == "terminal"`, schema parity for the visible fields |

## 8. Error Handling

| Scenario | Outcome |
|---|---|
| Missing / invalid tool args | existing schema-validation path |
| Blocked device path | non-fatal tool JSON error |
| Binary file read | non-fatal tool JSON error |
| Missing file | non-fatal tool JSON error, include similar files when practical |
| Sensitive or cross-profile write denied | non-fatal tool JSON error |
| Skill not found | non-fatal tool JSON error with available-skill hint |
| Ambiguous bare skill name | non-fatal tool JSON error naming the collisions |
| Plugin-qualified skill requested | non-fatal unsupported error in this phase |
| Traversal in `skill_view.file_path` | non-fatal tool JSON error |

User-input-driven failures should remain tool-result errors rather than crashing the loop.

## 9. Impact on Existing Code

| File | Change |
|---|---|
| `crates/hermes-agent/src/tools/bash.rs` | align public tool contract to Python `terminal` |
| `crates/hermes-agent/src/tools/files.rs` | new |
| `crates/hermes-agent/src/tools/skills.rs` | new |
| `crates/hermes-agent/src/tool_catalog.rs` | register `terminal`, `read_file`, `write_file`, `skills_list`, `skill_view` |
| `crates/hermes-agent/src/runtime_agent.rs` | resolve `skills_dir` once and pass to registry |
| `crates/hermes-agent/src/prompting.rs` | reuse shared `resolve_skills_dir()` helper |
| `crates/hermes-agent/src/lib.rs` | export the new tool types |
| `crates/hermes-agent/tests/*` | extend for schema + behavior parity |
| `hermes-skill-loader` | may need small API additions if Rust tools require richer frontmatter/linked-file helpers |

## 10. Follow-Up Backlog

- `patch` and `search_files`
- Full terminal backend parity with Python
- Plugin skill dispatch
- External skills dirs parity
- Read dedup / stale-read / cross-agent file-state parity
- Syntax/lint delta parity after writes
- Skill readiness metadata parity beyond the basic fields
