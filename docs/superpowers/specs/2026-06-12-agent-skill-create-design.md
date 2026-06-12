# Agent-Authored Skill Creation

**Date:** 2026-06-12
**Status:** Approved (pre-implementation)

## Goal

Add an LLM-callable tool that lets the agent turn a successful workflow into a
reusable `SKILL.md`. The tool is the missing first step in the project's
self-learning roadmap: skills are the agent's procedural memory, and today
the only way to create one is for a human to hand-author a file under
`$PERRY_HERMES_HOME/skills/`.

The new tool writes a single `SKILL.md` to a path under
`$PERRY_HERMES_HOME/skills/<name>/`, validates the frontmatter and the file
shape against the same rules the existing loader applies, and refuses to
overwrite an existing skill.

## Non-Goals

- No edit / patch / delete. The agent can update an existing skill via the
  existing `write_file` / `patch` tools; this PR only delivers `create`.
- No category path. New skills always live at
  `$PERRY_HERMES_HOME/skills/<name>/SKILL.md` (no nested `<category>/<name>/`).
- No supporting-file write. `references/`, `templates/`, `scripts/`, `assets/`
  are not touched by this tool. Agents that want to add a `references/foo.md`
  use `write_file` after `skill_create` returns.
- No hot-reload. A skill created during a session is **not** visible to
  `skills_list` / `skill_view` for the rest of that session. The system
  prompt tells the agent this explicitly.
- No security guard. perry_hermes does not gate the `terminal` tool today;
  writing a SKILL.md is morally equivalent to writing a markdown file. A
  guard is a separate concern, deferred until the rest of the project has
  one.
- No CLI slash command. The PR delivers the LLM tool only; a `/skill-create`
  CLI command can come later.
- No new public API for users to register skill creators. The new tool is
  wired into the default `build_registry` like every other built-in.

## Architecture

```text
perry-hermes-skill-tools
  └─ tools/skill_create.rs        (new — SkillCreateTool)
  └─ tools/mod.rs                  (modify — pub use SkillCreateTool)
  └─ lib.rs                        (modify — render_system_prompt_block
                                            mentions skill_create)

perry-hermes-agent
  └─ tool_catalog.rs               (modify — register SkillCreateTool
                                            in the "skills" toolset,
                                            gated by disabled_toolsets
                                            alongside SkillListTool /
                                            SkillViewTool)
```

`SkillCreateTool` is a sibling of `SkillListTool` and `SkillViewTool`, lives
in the same `tools/` directory, and reuses the frontmatter parser and
validators from `perry-hermes-skill-tools`. The toolset name is `"skills"`,
so `disabled_toolsets = ["skills"]` disables all three at once.

## Component 1: `SkillCreateTool`

```rust
// crates/hermes-skill-tools/src/tools/skill_create.rs

pub struct SkillCreateTool {
    skills_dir: PathBuf,
}
```

Constructor mirrors `SkillListTool::new` / `SkillViewTool::new`:

```rust
impl SkillCreateTool {
    pub fn new(skills_dir: PathBuf) -> Self { ... }
}
```

### Tool trait

- `name() -> "skill_create"`
- `description() -> "..."` (see below)
- `toolset() -> "skills"`
- `parameters_schema()` returns the schema below
- `is_async() -> false`
- `max_result_size_chars() -> None`
- `emoji() -> Some("📝")` (consistency with the rest of the skill tools; not
  load-bearing)

### Description string

```
"Create a new SKILL.md on disk under $PERRY_HERMES_HOME/skills/<name>/.
Refuses to overwrite an existing skill; use write_file or patch to update.
The new skill will be visible to skills_list / skill_view in the NEXT
session; it is not hot-reloaded in the current session."
```

### Parameters schema

```json
{
  "type": "object",
  "properties": {
    "name": {
      "type": "string",
      "description": "Skill directory name. Must be 1..=64 chars from [a-z0-9-], no XML brackets, no '..', and must match the frontmatter `name` field."
    },
    "content": {
      "type": "string",
      "description": "Full SKILL.md body. Must start with a YAML frontmatter block delimited by '---' lines, contain a `name` field equal to the `name` argument, contain a non-empty `description` (≤ 1024 chars, no XML brackets), and be ≤ 100_000 chars total."
    }
  },
  "required": ["name", "content"],
  "additionalProperties": false
}
```

## Component 2: Validation and Write Order

`execute` performs these steps in order. Any failure short-circuits with
`ToolOutput { content: json!({...success: false...}).to_string() }` and does
**not** touch the filesystem. All errors are reported as a string field
`error` and (where it helps the agent fix its input) a `field` hint.

1. **Argument shape.** `name` is a non-empty string; `content` is a non-empty
   string. Missing/wrong-type arguments surface as
   `error: "missing 'name'"` (or `"content"`), matching the rest of the
   tools.
2. **Name shape.** Reuse `validate::is_valid_category(name)`. This already
   enforces 1..=64 chars, `[a-z0-9-]`, no `<` / `>`. It also rejects `""`
   and uppercase / underscore / over-length inputs.
3. **Name contains `..`.** Explicit refusal (not covered by
   `is_valid_category`, which would happily accept `..foo`):
   `error: "name must not contain '..'"`. This blocks `..`-style path
   escape attempts even if a future validator becomes more permissive.
4. **Content size.** Reject `content.len() > 100_000`:
   `error: "content length N exceeds 100_000"`. Matches hermes-agent's
   `MAX_SKILL_CONTENT_CHARS`.
5. **Frontmatter parses.** Call `frontmatter::parse(content)`. On
   `None` (no opening fence or unterminated), return
   `error: "content must start with a YAML frontmatter block delimited by '---' lines"`.
   On `Value::Null` (frontmatter fence present but YAML invalid), return
   `error: "frontmatter is not valid YAML"`.
6. **Frontmatter is a mapping.** `error: "frontmatter must be a YAML mapping"`.
7. **`name` field present and equal.** `fm.get("name").and_then(|v| v.as_str())`
   must be `Some(name)`. If missing:
   `error: "frontmatter is missing required field 'name'"`. If present but
   mismatched:
   `error: "frontmatter name 'X' does not match directory name 'Y'"`,
   `field: "name"`. This enforces the loader's "directory basename must
   match frontmatter name" rule, so the file is loadable on the next
   session.
8. **`description` field present and valid.**
   `fm.get("description").and_then(|v| v.as_str())` must be
   `Some(d)` with `validate::is_valid_description(d)`. On missing:
   `error: "frontmatter is missing required field 'description'"`. On
   invalid: report the specific failure with `field: "description"`.
9. **Body present.** `body.trim().is_empty()` → refuse:
   `error: "skill body must not be empty"`. The loader is happy with empty
   bodies, but peer skills all have content, and a skill with no body is
   almost always a mistake.
10. **Collision.** If `<skills_dir>/<name>/SKILL.md` exists, refuse:
    ```json
    {
      "success": false,
      "error": "skill 'foo' already exists at <path>; use write_file or patch to update",
      "existing_path": "<path>"
    }
    ```
    Existence is checked on the file, not the directory, so a stray
    `references/` subdirectory from a future edit attempt does not block
    creation.
11. **Atomic write.**
    - `create_dir_all(<skills_dir>/<name>)` if the parent dir is missing.
    - `tempfile::NamedTempFile::new_in(<skills_dir>/<name>)`. The temp file
      is created in the target directory so the final `persist` is a
      same-filesystem rename.
    - `temp.write_all(content.as_bytes())` and `temp.flush()`.
    - `temp.persist(<skills_dir>/<name>/SKILL.md)` with `set_permissions`
      matching a regular file (0o644 on Unix). On `persist` failure, best
      effort `temp.reopen` / cleanup so no half-written file is left.
12. **Post-write sanity.** Re-read the persisted file with
    `frontmatter::parse` and re-apply `validate::*`. If the re-parse fails
    (it shouldn't — we just wrote the same bytes — but filesystem
    corruption / encoding surprise is real), return `success: false` with
    the diagnostic. The atomic-write contract means the failed file is
    already gone.

## Component 3: Success Return

```json
{
  "success": true,
  "name": "rust-error-formatting",
  "qualified_name": "rust-error-formatting",
  "category": null,
  "description": "Use when ..." ,
  "path": "/Users/.../.perry_hermes/skills/rust-error-formatting/SKILL.md",
  "size_bytes": 4321,
  "note": "Skill is on disk. It will be visible to skills_list / skill_view in the next session."
}
```

The `note` is the same wording the system prompt uses, so the model
sees consistent messaging in both places.

## Component 4: Wiring into `tool_catalog`

`build_registry` already handles the `"skills"` toolset pair:

```rust
if !disabled_toolsets.iter().any(|t| t == "skills") {
    reg = reg.register(Arc::new(SkillListTool::new(skills_dir.to_path_buf())));
    reg = reg.register(Arc::new(SkillViewTool::new(skills_dir.to_path_buf())));
}
```

The new tool is added in the same guard:

```rust
reg = reg.register(Arc::new(SkillCreateTool::new(skills_dir.to_path_buf())));
```

`skill_create` is on by default, like its siblings. Operators that want to
disable it disable the whole `skills` toolset.

## Component 5: System Prompt Update

`render_system_prompt_block` (in `crates/hermes-skill-tools/src/lib.rs`)
currently says:

```
Use the `skill_view` tool (or read the file directly with bash) to load a
skill's body when it is relevant to the user's request.
```

Add a second sentence directly under it:

```
Use the `skill_create` tool to record a successful workflow as a new
SKILL.md. Skills created in the current session are not visible to
`skills_list` / `skill_view` until the next session starts.
```

The `render_system_prompt_block` unit tests are extended to assert both
the `skill_view` and `skill_create` mentions are present in the rendered
block.

## Test Plan

All tests are unit tests next to the code they cover.

### `crates/hermes-skill-tools/src/tools/skill_create.rs`

- `creates_skill_md_on_disk` — happy path: tempdir as `skills_dir`, call the
  tool with a valid `name` and a valid `content`, assert the file exists at
  `<tmp>/<name>/SKILL.md` and its bytes equal `content`.
- `writes_to_existing_skills_dir` — `skills_dir` already exists with a
  peer skill in it; new skill does not clobber the peer; the new
  `SKILL.md` is in its own subdirectory.
- `rejects_collision` — call the tool, then call again with the same name;
  second call returns `success: false`, `error` mentions "already exists",
  and `existing_path` matches the path of the first write.
- `rejects_invalid_name_uppercase` — `name = "Foo"` returns
  `success: false`, error mentions validation.
- `rejects_invalid_name_with_dotdot` — `name = ".."` returns
  `success: false`, error mentions "must not contain '..'".
- `rejects_invalid_name_oversize` — 65 chars returns
  `success: false`, error mentions length.
- `rejects_invalid_name_with_xml_bracket` — `name = "a<b"` returns
  `success: false`.
- `rejects_missing_frontmatter` — `content = "no fence"` returns
  `success: false`, error mentions frontmatter.
- `rejects_invalid_yaml_frontmatter` — fence present, body of YAML
  malformed, returns `success: false`, error mentions YAML.
- `rejects_frontmatter_name_mismatch` — `name = "foo"`, frontmatter
  `name: bar` returns `success: false`, error mentions mismatch,
  `field: "name"`.
- `rejects_missing_description` — frontmatter has no `description` returns
  `success: false`, error mentions description.
- `rejects_oversized_description` — `description` 1025 chars returns
  `success: false`, error mentions length, `field: "description"`.
- `rejects_oversized_content` — `content` 100_001 chars returns
  `success: false`, error mentions 100_000.
- `rejects_empty_body` — frontmatter valid, body whitespace only returns
  `success: false`, error mentions empty body.
- `atomic_write_does_not_leave_tempfile` — on success, no temp files
  remain in the skill directory; the only file is `SKILL.md`.
- `success_return_includes_path_and_note` — assert the success JSON has
  `path`, `size_bytes`, and a `note` mentioning the next-session limitation.

### `crates/hermes-agent/src/tool_catalog.rs`

- `registers_skill_create_tool_by_default` — `build_registry(&[], ...)`
  followed by `registry.get("skill_create")` returns `Some`.
- `disabled_skills_toolset_removes_skill_create` —
  `build_registry(&["skills".into()], ...)` followed by
  `registry.get("skill_create")` returns `None`. Same for `skill_list` and
  `skill_view` (existing tests already cover those; this one extends the
  pattern).

### `crates/hermes-skill-tools/src/lib.rs`

- `render_block_mentions_skill_create_tool` — extend the existing
  `render_block_includes_forward_reference_to_skill_view_tool` test (or
  add a sibling) to assert the new sentence is rendered.

## Cross-Cutting Notes

- The new tool does not need a `discover` step. `perry-hermes-skill-tools`
  already exposes `tools::mod` as the place for tool implementations; the
  only registration point is `tool_catalog::build_registry`, which we
  update in the same change.
- `tempfile` is already a dev-dep / workspace dep; it is pulled in by
  the existing test suite. The new code uses `tempfile::NamedTempFile`
  for atomic write, consistent with how the rest of the test code uses
  `tempfile::tempdir()`.
- No new error type. All errors are returned as `ToolOutput` JSON with
  `success: false`, matching every other tool in `hermes-skill-tools`.
- The tool's `description` is the agent's primary onboarding for the
  next-session limitation. The system-prompt sentence is the secondary
  reminder. Both should be kept in sync; this spec pins both wordings.
- The `name` field is reused as the directory name. The loader's
  "directory basename must equal frontmatter name" invariant means
  forcing the two equal is exactly the right behavior — and refusing
  to enforce it would produce a skill that silently fails to load on
  the next session.

## Acceptance

- [ ] `cargo fmt --all`, `cargo test --workspace`,
      `cargo clippy --all-targets --all-features -- -D warnings`,
      `cargo doc --no-deps` all green.
- [ ] `SkillCreateTool` exists, is registered by default, is disabled by
      `disabled_toolsets = ["skills"]`, and writes a loadable `SKILL.md`
      on disk on success.
- [ ] `skills_list` / `skill_view` behavior in the same session is
      unchanged (no hot-reload, no scan reschedule).
- [ ] System prompt mentions `skill_create` and the next-session
      limitation; the existing `skill_view` mention is preserved.
- [ ] No new public API; the only new public surface is
      `perry_hermes_skill_tools::tools::SkillCreateTool`, in the same
      `pub use` style as `SkillListTool` / `SkillViewTool`.
- [ ] `AGENTS.md` "Architecture" table grows no new row — the new tool
      lives in the existing `perry-hermes-skill-tools` row.

## Out of Scope / Future Work

These are flagged here so a future contributor does not bundle them
into the implementation PR.

- `skill_edit` / `skill_patch` / `skill_delete` (separate PR, after
  curator work; see the `curator` reference in
  `docs/history/hermes-comparison.md`).
- Category-aware creation path (decide on first concrete need; the
  current loader already supports `<category>/<name>/` so adding it later
  is additive).
- Supporting-file sub-actions (a separate `skill_write_file` would mirror
  hermes-agent's `skill_manager_tool` action enum; out of scope here).
- Hot-reload of the skill registry within a session (needs cross-cutting
  coordination between the tool, the agent loop's prompt context, and
  each platform adapter; revisit when the prompt context block trait
  stabilizes).
- Security guard for agent-authored skills (mirror hermes-agent's
  `skills_guard_agent_created` config; defer until the rest of the
  project has an equivalent guard).
- CLI slash command `/skill-create` (parallel feature; out of scope).
