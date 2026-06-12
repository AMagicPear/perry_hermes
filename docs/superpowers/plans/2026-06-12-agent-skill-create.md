# Agent-Authored Skill Creation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an LLM-callable `skill_create` tool that writes a validated `SKILL.md` to `$PERRY_HERMES_HOME/skills/<name>/` and refuses to overwrite an existing skill.

**Architecture:** New `SkillCreateTool` struct in `perry-hermes-skill-tools` mirroring the existing `SkillListTool` / `SkillViewTool` shape. Validation reuses `frontmatter::parse` and `validate::{is_valid_category, is_valid_description}`. Atomic write via `tempfile::NamedTempFile` + `persist_noclobber`. Tool is registered into the `"skills"` toolset, alongside the read-only tools. System prompt in `render_system_prompt_block` gains a sentence that advertises the new tool and the next-session limitation.

**Tech Stack:** Rust 1.95, async-trait, tempfile (already a regular dep on `perry-hermes-skill-tools`), serde_json, tokio (test runtime).

**Reference spec:** [docs/superpowers/specs/2026-06-12-agent-skill-create-design.md](../specs/2026-06-12-agent-skill-create-design.md)

**Reference files to read before starting:**
- `crates/hermes-skill-tools/src/lib.rs` — `Skill` struct, `load_all`, `frontmatter::parse`
- `crates/hermes-skill-tools/src/validate.rs` — `is_valid_category`, `is_valid_description`
- `crates/hermes-skill-tools/src/tools/skill_view.rs` — reference tool implementation pattern
- `crates/hermes-skill-tools/src/tools/skill_list.rs` — second reference
- `crates/hermes-skill-tools/src/tools/mod.rs` — `pub use` re-exports
- `crates/hermes-skill-tools/tests/skills.rs` — integration test pattern (`#[tokio::test]`, `TempDir`, `ctx()`, `parse()`)
- `crates/hermes-agent/src/tool_catalog.rs` — `build_registry`, `disabled_toolsets` filter, existing test pattern

---

## File Structure

**Create:**
- `crates/hermes-skill-tools/src/tools/skill_create.rs` — `SkillCreateTool` struct + `Tool` impl + `#[cfg(test)] mod tests` (in-source unit tests for validation helpers)

**Modify:**
- `crates/hermes-skill-tools/src/tools/mod.rs` — declare `mod skill_create;` (private), `pub use skill_create::SkillCreateTool;`
- `crates/hermes-skill-tools/src/lib.rs` — `render_system_prompt_block` adds the `skill_create` mention sentence
- `crates/hermes-skill-tools/tests/skills.rs` — append integration tests for `SkillCreateTool`
- `crates/hermes-agent/src/tool_catalog.rs` — `build_registry` registers `SkillCreateTool` in the `"skills"` toolset; extend the existing `skills_toolset_disables_list_and_view` test and `default_registry_includes_all_tools` test

`SkillCreateTool` lives in `perry-hermes-skill-tools` next to its siblings; the in-source unit tests cover pure validation helpers (e.g. `validate_name`) so they run with `cargo test -p perry-hermes-skill-tools`, and the integration tests in `tests/skills.rs` cover the `Tool::execute` end-to-end behavior so they exercise the same harness as the existing `skill_list` / `skill_view` tests.

---

## Task 1: Tool Skeleton + Toolset Registration

**Files:**
- Create: `crates/hermes-skill-tools/src/tools/skill_create.rs`
- Modify: `crates/hermes-skill-tools/src/tools/mod.rs`
- Modify: `crates/hermes-agent/src/tool_catalog.rs`

- [ ] **Step 1: Write failing test for `SkillCreateTool` registration by default**

Add to the bottom of `crates/hermes-agent/src/tool_catalog.rs` `mod tests`:

```rust
    #[test]
    fn skills_toolset_includes_skill_create_by_default() {
        let registry = build_registry(&[], &test_skills_dir(), None);
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(
            names.iter().any(|n| n == "skill_create"),
            "skill_create should be registered by default, got: {names:?}"
        );
    }

    #[test]
    fn skills_toolset_disables_skill_create() {
        let registry = build_registry(&["skills".to_string()], &test_skills_dir(), None);
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(
            !names.iter().any(|n| n == "skill_create"),
            "skill_create should be removed when skills toolset is disabled"
        );
    }
```

Also extend the existing `default_registry_includes_all_tools` test in the same file by adding one more assertion:

```rust
        assert!(names.iter().any(|n| n == "skill_create"));
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p perry-hermes-agent --test '*' skills_toolset 2>&1 | tail -30`
Expected: FAIL — `SkillCreateTool` not in scope, `skill_create` not registered.

- [ ] **Step 3: Create the tool skeleton**

Create `crates/hermes-skill-tools/src/tools/skill_create.rs`:

```rust
//! `skill_create` — LLM-callable tool for authoring new SKILL.md files.
//!
//! See [`docs/superpowers/specs/2026-06-12-agent-skill-create-design.md`][spec]
//! for the design. The tool writes a single SKILL.md under
//! `$PERRY_HERMES_HOME/skills/<name>/`, validates frontmatter and body
//! against the same rules the loader applies, and refuses to overwrite
//! an existing skill.
//!
//! [spec]: ../../../../docs/superpowers/specs/2026-06-12-agent-skill-create-design.md

use std::path::PathBuf;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const SKILL_CREATE_DESCRIPTION: &str = "Create a new SKILL.md on disk under \
$PERRY_HERMES_HOME/skills/<name>/. Refuses to overwrite an existing skill; \
use write_file or patch to update. The new skill will be visible to \
skills_list / skill_view in the NEXT session; it is not hot-reloaded in \
the current session.";

const MAX_CONTENT_CHARS: usize = 100_000;

pub struct SkillCreateTool {
    skills_dir: PathBuf,
}

impl SkillCreateTool {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }
}

#[async_trait]
impl Tool for SkillCreateTool {
    fn name(&self) -> &str {
        "skill_create"
    }

    fn description(&self) -> &str {
        SKILL_CREATE_DESCRIPTION
    }

    fn toolset(&self) -> &'static str {
        "skills"
    }

    fn emoji(&self) -> Option<&str> {
        Some("📝")
    }

    fn parameters_schema(&self) -> Value {
        json!({
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
        })
    }

    async fn execute(
        &self,
        _args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        // Implemented in Task 2.
        Ok(ToolOutput {
            content: json!({
                "success": false,
                "error": "skill_create not yet implemented",
            })
            .to_string(),
        })
    }
}
```

- [ ] **Step 4: Wire it into the tool registry**

In `crates/hermes-skill-tools/src/tools/mod.rs`, add `mod skill_create;` next to the existing `mod skill_view;` declaration, and add `pub use skill_create::SkillCreateTool;` next to the existing `pub use skill_view::SkillViewTool;` re-export:

```rust
pub mod files;
mod linked_files;
pub mod memory;
pub mod process;
pub mod process_registry;
mod skill_create;
mod skill_list;
mod skill_view;
pub mod terminal;

pub use files::{PatchTool, ReadFileTool, SearchFilesTool, WriteFileTool};
pub use process::ProcessTool;
pub use skill_create::SkillCreateTool;
pub use skill_list::SkillListTool;
pub use skill_view::SkillViewTool;
pub use terminal::{BashTool, TERMINAL_TOOL_DESCRIPTION};
```

In `crates/hermes-agent/src/tool_catalog.rs`, add `SkillCreateTool` to the `perry_hermes_skill_tools::tools::` import line:

```rust
use perry_hermes_skill_tools::tools::{
    BashTool, PatchTool, ProcessTool, ReadFileTool, SearchFilesTool, SkillCreateTool,
    SkillListTool, SkillViewTool, WriteFileTool,
};
```

Then inside `build_registry`, in the `if !disabled_toolsets.iter().any(|s| s == "skills")` block, register the new tool:

```rust
    if !disabled_toolsets.iter().any(|s| s == "skills") {
        reg = reg.register(Arc::new(SkillListTool::new(skills_dir.to_path_buf())));
        reg = reg.register(Arc::new(SkillViewTool::new(skills_dir.to_path_buf())));
        reg = reg.register(Arc::new(SkillCreateTool::new(skills_dir.to_path_buf())));
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p perry-hermes-agent --test '*' skills_toolset 2>&1 | tail -30`
Expected: PASS — `skill_create` is registered by default and removed when the `"skills"` toolset is disabled. The `default_registry_includes_all_tools` test also passes.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-skill-tools/src/tools/skill_create.rs \
        crates/hermes-skill-tools/src/tools/mod.rs \
        crates/hermes-agent/src/tool_catalog.rs
git commit -m "feat(skills): add SkillCreateTool skeleton registered in skills toolset"
```

---

## Task 2: Happy Path (Create + Atomic Write)

**Files:**
- Modify: `crates/hermes-skill-tools/src/tools/skill_create.rs`
- Modify: `crates/hermes-skill-tools/tests/skills.rs`

- [ ] **Step 1: Write failing integration test for happy path**

Append to `crates/hermes-skill-tools/tests/skills.rs`:

```rust
// ---------------------------------------------------------------------------
// SkillCreateTool
// ---------------------------------------------------------------------------

fn read_skill_md(dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(name).join("SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {:?} failed: {e}", path))
}

#[tokio::test]
async fn skill_create_writes_a_valid_skill_md_to_disk() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    let tool = SkillCreateTool::new(skills_dir.clone());

    let body = "# Rust error formatting\n\n## Overview\nUse thiserror for libraries, anyhow for apps.\n";
    let content = format!(
        "---\nname: rust-error-formatting\ndescription: Use when formatting errors in Rust crates.\n---\n\n{body}"
    );

    let out = tool
        .execute(
            json!({ "name": "rust-error-formatting", "content": content }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("create should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true));
    assert_eq!(v["name"].as_str(), Some("rust-error-formatting"));
    assert_eq!(v["qualified_name"].as_str(), Some("rust-error-formatting"));
    assert!(v["category"].is_null());
    assert_eq!(
        v["description"].as_str(),
        Some("Use when formatting errors in Rust crates.")
    );
    assert!(v["path"].as_str().unwrap().ends_with("rust-error-formatting/SKILL.md"));
    assert_eq!(v["size_bytes"].as_u64(), Some(content.len() as u64));
    assert!(v["note"].as_str().unwrap().contains("next session"));

    // File on disk matches what we passed in, byte-for-byte.
    let on_disk = read_skill_md(&skills_dir, "rust-error-formatting");
    assert_eq!(on_disk, content, "on-disk file should equal input content");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p perry-hermes-skill-tools --test skills skill_create_writes 2>&1 | tail -20`
Expected: FAIL — `success: false, error: "skill_create not yet implemented"`.

- [ ] **Step 3: Implement happy path inside `execute`**

Replace the body of `SkillCreateTool::execute` in `crates/hermes-skill-tools/src/tools/skill_create.rs` with:

```rust
    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: pull out the two required args.
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return Ok(failure("missing 'name'", None));
        };
        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return Ok(failure("missing 'content'", None));
        };

        // Step 2: name shape (1..=64 chars, [a-z0-9-], no XML brackets, no "..").
        if name.contains("..") {
            return Ok(failure("name must not contain '..'", Some("name")));
        }
        if !crate::validate::is_valid_category(name) {
            return Ok(failure(
                "name must be 1..=64 chars from [a-z0-9-] with no XML brackets",
                Some("name"),
            ));
        }

        // Step 3: content size.
        if content.len() > MAX_CONTENT_CHARS {
            return Ok(failure(
                format!("content length {} exceeds {MAX_CONTENT_CHARS}"),
                Some("content"),
            ));
        }

        // Step 4: frontmatter parses.
        let Some((fm, body)) = crate::frontmatter::parse(content) else {
            return Ok(failure(
                "content must start with a YAML frontmatter block delimited by '---' lines",
                Some("content"),
            ));
        };
        if fm.is_null() {
            return Ok(failure("frontmatter is not valid YAML", Some("content")));
        }
        if !fm.is_mapping() {
            return Ok(failure("frontmatter must be a YAML mapping", Some("content")));
        }

        // Step 5: frontmatter `name` matches the directory name.
        let Some(fm_name) = fm.get("name").and_then(|v| v.as_str()) else {
            return Ok(failure(
                "frontmatter is missing required field 'name'",
                Some("name"),
            ));
        };
        if fm_name != name {
            return Ok(failure(
                format!("frontmatter name '{fm_name}' does not match directory name '{name}'"),
                Some("name"),
            ));
        }

        // Step 6: frontmatter `description` is valid.
        let Some(description) = fm.get("description").and_then(|v| v.as_str()) else {
            return Ok(failure(
                "frontmatter is missing required field 'description'",
                Some("description"),
            ));
        };
        if !crate::validate::is_valid_description(description) {
            return Ok(failure(
                "description must be 1..=1024 chars and contain no XML brackets",
                Some("description"),
            ));
        }

        // Step 7: body present.
        if body.trim().is_empty() {
            return Ok(failure("skill body must not be empty", Some("content")));
        }

        // Step 8: collision check.
        let target = self.skills_dir.join(name).join("SKILL.md");
        if target.exists() {
            return Ok(ToolOutput {
                content: json!({
                    "success": false,
                    "error": format!(
                        "skill '{name}' already exists at {}; use write_file or patch to update",
                        target.display()
                    ),
                    "existing_path": target.to_string_lossy(),
                })
                .to_string(),
            });
        }

        // Step 9: atomic write.
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Ok(failure(format!("create_dir_all failed: {e}"), None));
            }
        }
        let write_result = (|| -> std::io::Result<()> {
            let tmp = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
            std::fs::write(tmp.path(), content.as_bytes())?;
            tmp.persist(&target).map_err(|e| std::io::Error::new(e.error.kind(), e.error))?;
            Ok(())
        })();
        if let Err(e) = write_result {
            return Ok(failure(format!("write failed: {e}"), None));
        }

        // Step 10: post-write sanity. Re-parse and re-validate.
        let written = match std::fs::read_to_string(&target) {
            Ok(s) => s,
            Err(e) => return Ok(failure(format!("post-write read failed: {e}"), None)),
        };
        if crate::frontmatter::parse(&written).is_none() {
            return Ok(failure(
                "post-write re-parse failed: SKILL.md on disk has no frontmatter",
                None,
            ));
        }

        Ok(ToolOutput {
            content: json!({
                "success": true,
                "name": name,
                "qualified_name": name,
                "category": Value::Null,
                "description": description,
                "path": target.to_string_lossy(),
                "size_bytes": content.len(),
                "note": "Skill is on disk. It will be visible to skills_list / skill_view in the next session.",
            })
            .to_string(),
        })
    }
```

Also add a private helper at the top of the file (after the `MAX_CONTENT_CHARS` const):

```rust
fn failure<S: Into<String>>(error: S, field: Option<&'static str>) -> ToolOutput {
    let mut payload = serde_json::Map::new();
    payload.insert("success".into(), Value::Bool(false));
    payload.insert("error".into(), Value::String(error.into()));
    if let Some(f) = field {
        payload.insert("field".into(), Value::String(f.to_string()));
    }
    ToolOutput {
        content: Value::Object(payload).to_string(),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p perry-hermes-skill-tools --test skills skill_create_writes 2>&1 | tail -20`
Expected: PASS — happy path writes the file and returns the success JSON.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-skill-tools/src/tools/skill_create.rs \
        crates/hermes-skill-tools/tests/skills.rs
git commit -m "feat(skills): implement skill_create happy path with validation + atomic write"
```

---

## Task 3: Argument Shape and Name Validation

**Files:**
- Modify: `crates/hermes-skill-tools/tests/skills.rs`

These tests cover the failure modes handled by steps 1-3 of `execute` (argument shape, name shape, content size).

- [ ] **Step 1: Write failing tests for argument + name + content-size validation**

Append to `crates/hermes-skill-tools/tests/skills.rs`:

```rust
fn err(out: &perry_hermes_core::tool::ToolOutput) -> serde_json::Value {
    let v = parse(out);
    assert_eq!(
        v["success"].as_bool(),
        Some(false),
        "expected success=false, got: {v}"
    );
    v
}

#[tokio::test]
async fn skill_create_rejects_missing_name() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let out = tool
        .execute(
            json!({ "content": "---\nname: x\ndescription: y\n---\nbody" }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("missing 'name'"));
}

#[tokio::test]
async fn skill_create_rejects_missing_content() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let out = tool
        .execute(json!({ "name": "foo" }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("missing 'content'"));
}

#[tokio::test]
async fn skill_create_rejects_invalid_name_uppercase() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: Foo\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "Foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("name"));
    assert_eq!(v["field"].as_str(), Some("name"));
}

#[tokio::test]
async fn skill_create_rejects_name_with_dotdot() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: ..\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "..", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("'..'"));
    assert_eq!(v["field"].as_str(), Some("name"));
}

#[tokio::test]
async fn skill_create_rejects_oversize_name() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let long = "a".repeat(65);
    let content = format!("---\nname: {long}\ndescription: x\n---\nbody\n");
    let out = tool
        .execute(json!({ "name": long, "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("name"));
    assert_eq!(v["field"].as_str(), Some("name"));
}

#[tokio::test]
async fn skill_create_rejects_name_with_xml_bracket() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: a<b\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "a<b", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("name"));
    assert_eq!(v["field"].as_str(), Some("name"));
}

#[tokio::test]
async fn skill_create_rejects_oversize_content() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let big_body = "x".repeat(100_001);
    let content = format!("---\nname: foo\ndescription: y\n---\n{big_body}");
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("100_000"));
    assert_eq!(v["field"].as_str(), Some("content"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p perry-hermes-skill-tools --test skills skill_create_rejects 2>&1 | tail -30`
Expected: PASS — all 7 tests pass against the validation pipeline implemented in Task 2.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-skill-tools/tests/skills.rs
git commit -m "test(skills): cover skill_create argument, name, and content-size validation"
```

---

## Task 4: Frontmatter and Body Validation

**Files:**
- Modify: `crates/hermes-skill-tools/tests/skills.rs`

These tests cover the failure modes handled by steps 4-7 of `execute` (frontmatter parsing, name/description match, body non-empty).

- [ ] **Step 1: Write failing tests for frontmatter + body validation**

Append to `crates/hermes-skill-tools/tests/skills.rs`:

```rust
#[tokio::test]
async fn skill_create_rejects_missing_frontmatter() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let out = tool
        .execute(
            json!({ "name": "foo", "content": "no fence here" }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("frontmatter"));
    assert_eq!(v["field"].as_str(), Some("content"));
}

#[tokio::test]
async fn skill_create_rejects_invalid_yaml_frontmatter() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    // Unclosed flow sequence — YAML parse fails.
    let content = "---\nname: [unclosed\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("YAML"));
    assert_eq!(v["field"].as_str(), Some("content"));
}

#[tokio::test]
async fn skill_create_rejects_non_mapping_frontmatter() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    // Valid YAML, but a string scalar rather than a mapping.
    let content = "---\njust-a-string\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("mapping"));
    assert_eq!(v["field"].as_str(), Some("content"));
}

#[tokio::test]
async fn skill_create_rejects_frontmatter_name_mismatch() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: bar\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("does not match"));
    assert_eq!(v["field"].as_str(), Some("name"));
}

#[tokio::test]
async fn skill_create_rejects_missing_description() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: foo\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("description"));
    assert_eq!(v["field"].as_str(), Some("description"));
}

#[tokio::test]
async fn skill_create_rejects_oversize_description() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let long_desc = "x".repeat(1025);
    let content = format!("---\nname: foo\ndescription: {long_desc}\n---\nbody\n");
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("description"));
    assert_eq!(v["field"].as_str(), Some("description"));
}

#[tokio::test]
async fn skill_create_rejects_empty_body() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let content = "---\nname: foo\ndescription: x\n---\n   \n  \n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("body"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p perry-hermes-skill-tools --test skills skill_create_rejects 2>&1 | tail -30`
Expected: PASS — all 7 new tests pass; combined with Task 3 we now have 14 negative-path tests.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-skill-tools/tests/skills.rs
git commit -m "test(skills): cover skill_create frontmatter and body validation"
```

---

## Task 5: Collision Rejection and Atomic-Write Cleanliness

**Files:**
- Modify: `crates/hermes-skill-tools/tests/skills.rs`

These tests cover the failure mode at step 8 of `execute` (collision) and a property test for the atomic write (no temp files left behind).

- [ ] **Step 1: Write failing tests for collision + atomic-write cleanliness**

Append to `crates/hermes-skill-tools/tests/skills.rs`:

```rust
#[tokio::test]
async fn skill_create_rejects_existing_skill() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    let tool = SkillCreateTool::new(skills_dir.clone());

    // Seed an existing skill at the same path.
    write_skill(&skills_dir, "foo", "old", "old body");

    let content = "---\nname: foo\ndescription: new desc\n---\nnew body\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("already exists"));
    assert!(v["error"].as_str().unwrap().contains("write_file or patch"));
    assert_eq!(v["existing_path"].as_str().unwrap(), skills_dir.join("foo").join("SKILL.md").to_string_lossy());

    // The pre-existing SKILL.md must be unchanged.
    let on_disk = read_skill_md(&skills_dir, "foo");
    assert!(on_disk.contains("old desc"), "pre-existing skill must be preserved, got: {on_disk}");
    assert!(!on_disk.contains("new body"), "collision must not overwrite, got: {on_disk}");
}

#[tokio::test]
async fn skill_create_atomic_write_leaves_no_tempfile() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    let tool = SkillCreateTool::new(skills_dir.clone());

    let content = "---\nname: foo\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("create should succeed");
    assert_eq!(parse(&out)["success"].as_bool(), Some(true));

    let skill_dir = skills_dir.join("foo");
    let mut entries: Vec<_> = std::fs::read_dir(&skill_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["SKILL.md".to_string()],
        "atomic write should leave only SKILL.md, got: {entries:?}"
    );
}

#[tokio::test]
async fn skill_create_creates_skills_dir_when_missing() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    assert!(!skills_dir.exists(), "precondition: skills dir absent");
    let tool = SkillCreateTool::new(skills_dir.clone());

    let content = "---\nname: foo\ndescription: x\n---\nbody\n";
    let out = tool
        .execute(json!({ "name": "foo", "content": content }), ctx(), CancellationToken::new())
        .await
        .expect("create should succeed");
    assert_eq!(parse(&out)["success"].as_bool(), Some(true));
    assert!(skills_dir.is_dir(), "skills dir should be created");
    assert!(skills_dir.join("foo").join("SKILL.md").is_file(), "SKILL.md should exist");
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p perry-hermes-skill-tools --test skills skill_create_ 2>&1 | tail -30`
Expected: PASS — 3 new tests pass; the full `skill_create_*` suite (18 tests total) is green.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-skill-tools/tests/skills.rs
git commit -m "test(skills): cover skill_create collision rejection and atomic-write cleanliness"
```

---

## Task 6: System Prompt Update

**Files:**
- Modify: `crates/hermes-skill-tools/src/lib.rs`

- [ ] **Step 1: Write failing test for the new system-prompt sentence**

In `crates/hermes-skill-tools/src/lib.rs`, find the existing `render_block_includes_forward_reference_to_skill_view_tool` test (in the bottom `mod tests`). Add a sibling test directly after it:

```rust
    #[test]
    fn render_block_mentions_skill_create_tool() {
        let skills = vec![make_skill("foo", None, "x")];
        let block = render_system_prompt_block(&skills);
        assert!(
            block.contains("skill_create"),
            "system prompt block should advertise skill_create, got: {block}"
        );
        assert!(
            block.contains("next session"),
            "system prompt should mention next-session limitation, got: {block}"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p perry-hermes-skill-tools --lib render_block_mentions_skill_create_tool 2>&1 | tail -20`
Expected: FAIL — the rendered block does not yet mention `skill_create`.

- [ ] **Step 3: Update `render_system_prompt_block`**

In `crates/hermes-skill-tools/src/lib.rs`, in the body of `render_system_prompt_block`, the existing leading sentence currently reads:

```rust
        "The following skills are available. Each skill is a directory containing a SKILL.md file with detailed instructions.\n\
         Use the `skill_view` tool (or read the file directly with bash) to load a skill's body when it is relevant to the user's request.\n\n\
         Available skills:\n",
```

Replace it with (keep everything else in `render_system_prompt_block` identical):

```rust
        "The following skills are available. Each skill is a directory containing a SKILL.md file with detailed instructions.\n\
         Use the `skill_view` tool (or read the file directly with bash) to load a skill's body when it is relevant to the user's request.\n\
         Use the `skill_create` tool to record a successful workflow as a new SKILL.md. Skills created in the current session are not visible to `skills_list` / `skill_view` until the next session starts.\n\n\
         Available skills:\n",
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p perry-hermes-skill-tools --lib render_block 2>&1 | tail -20`
Expected: PASS — both the new test and the existing `render_block_includes_forward_reference_to_skill_view_tool` test pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-skill-tools/src/lib.rs
git commit -m "feat(skills): advertise skill_create in system prompt with next-session note"
```

---

## Task 7: Final Acceptance

**Files:** none (run the full check suite).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff. If `cargo fmt` produced changes, run `git diff` to confirm they are formatting-only, then commit them as `style: cargo fmt`.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: all tests pass. The 18 new `skill_create_*` integration tests in `perry-hermes-skill-tools/tests/skills.rs` should be visible in the output. The 2 new `skills_toolset_includes_skill_create_by_default` / `skills_toolset_disables_skill_create` tests in `perry-hermes-agent`'s `tool_catalog.rs` should be visible. The new `render_block_mentions_skill_create_tool` test in `perry-hermes-skill-tools/src/lib.rs` should be visible. No pre-existing tests should regress.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -40`
Expected: zero warnings. If clippy suggests changes, fix them inline and amend the relevant commit (or add a new `chore: clippy` commit if the change is meaningful).

- [ ] **Step 4: Doc**

Run: `cargo doc --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links. The `///` doc comment on `SkillCreateTool` references a relative spec path; if rustdoc complains, drop the link target down to just the file name and add a plain "see the spec at this path" sentence.

- [ ] **Step 5: Final commit (only if any fmt/clippy fixes were needed)**

If any of the steps above required a fix, commit those changes:

```bash
git add -A
git commit -m "chore(skills): apply fmt/clippy cleanups for skill_create"
```

If no fixes were needed, skip this step.

- [ ] **Step 6: Hand-off**

Report:
- New test count: 18 integration tests + 2 unit tests in tool_catalog + 1 in lib.rs.
- Files touched: 4 (`skill_create.rs` new, `tools/mod.rs`, `tool_catalog.rs`, `lib.rs`, plus `tests/skills.rs`).
- Public API surface: one new `pub use SkillCreateTool`, in the existing `perry_hermes_skill_tools::tools` re-export.
- Behavior: `skill_create` writes a validated SKILL.md, refuses to overwrite, and is disabled alongside the rest of the skills toolset.
- Known limitation: skills created mid-session are not visible to `skills_list` / `skill_view` until the next session. The system prompt and the tool's `note` field both surface this.

---
