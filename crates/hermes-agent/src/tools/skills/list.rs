use std::path::PathBuf;

use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SKILLS_LIST_DESCRIPTION: &str =
    "List available skills (name + description). Use skill_view(name) to load full content.";

pub struct SkillListTool {
    skills_dir: PathBuf,
}

impl SkillListTool {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }
}

#[async_trait]
impl Tool for SkillListTool {
    fn name(&self) -> &str {
        "skills_list"
    }

    fn description(&self) -> &str {
        SKILLS_LIST_DESCRIPTION
    }

    fn toolset(&self) -> &'static str {
        "skills"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Optional category filter to narrow results"
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let category_filter = args
            .get("category")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase());

        if !self.skills_dir.exists() {
            let _ = std::fs::create_dir_all(&self.skills_dir);
        }

        let skills = match hermes_skills::load_all(&self.skills_dir) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({
                        "success": false,
                        "error": format!("failed to scan skills dir: {e}"),
                    })
                    .to_string(),
                });
            }
        };

        let mut filtered: Vec<&hermes_skills::Skill> = skills
            .iter()
            .filter(|s| match &category_filter {
                Some(c) => s.category.as_deref().map(|x| x.to_ascii_lowercase()) == Some(c.clone()),
                None => true,
            })
            .collect();
        filtered.sort_by(|a, b| {
            let ka = (a.category.clone().unwrap_or_default(), a.name.clone());
            let kb = (b.category.clone().unwrap_or_default(), b.name.clone());
            ka.cmp(&kb)
        });

        let mut categories: Vec<String> =
            skills.iter().filter_map(|s| s.category.clone()).collect();
        categories.sort();
        categories.dedup();

        let entries: Vec<Value> = filtered
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "category": s.category,
                    "description": s.description,
                })
            })
            .collect();

        Ok(ToolOutput {
            content: json!({
                "success": true,
                "count": entries.len(),
                "categories": categories,
                "skills": entries,
                "hint": "Use skill_view(name) to see full content, tags, and linked files",
            })
            .to_string(),
        })
    }
}
