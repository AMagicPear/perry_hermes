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
