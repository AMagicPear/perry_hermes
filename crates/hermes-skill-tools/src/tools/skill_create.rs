//! `skill_create` — LLM-callable tool for authoring new SKILL.md files.
//!
//! See the design spec at
//! `docs/superpowers/specs/2026-06-12-agent-skill-create-design.md`. The
//! tool writes a single SKILL.md under
//! `$PERRY_HERMES_HOME/skills/<name>/`, validates frontmatter and body
//! against the same rules the loader applies, and refuses to overwrite
//! an existing skill.

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
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        // Step 1: pull out the two required args.
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(failure("missing 'name'", None)),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(failure("missing 'content'", None)),
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
                format!(
                    "content length {} exceeds {MAX_CONTENT_CHARS}",
                    content.len()
                ),
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
            return Ok(failure(
                "frontmatter must be a YAML mapping",
                Some("content"),
            ));
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
            tmp.persist(&target)
                .map_err(|e| std::io::Error::new(e.error.kind(), e.error))?;
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
}
