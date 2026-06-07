use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::linked_files::discover_linked_files;

const SKILL_VIEW_DESCRIPTION: &str = "Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files (references, templates, scripts). First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path parameter.";

pub struct SkillViewTool {
    skills_dir: PathBuf,
}

impl SkillViewTool {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }
}

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn description(&self) -> &str {
        SKILL_VIEW_DESCRIPTION
    }

    fn toolset(&self) -> &'static str {
        "skills"
    }

    fn parameters_schema(&self) -> Value {
        json!({
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
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'name'".into()))?;
        let file_path = args.get("file_path").and_then(|v| v.as_str());

        if name.contains(':') {
            return Ok(ToolOutput {
                content: json!({
                    "success": false,
                    "error": "Plugin-qualified names (e.g. 'plugin:skill') are not supported in this build. Use a local skill name.",
                })
                .to_string(),
            });
        }

        if !self.skills_dir.exists() {
            return Ok(ToolOutput {
                content: json!({
                    "success": false,
                    "error": format!("Skills directory does not exist: {}", self.skills_dir.display()),
                })
                .to_string(),
            });
        }

        let skills = match hermes_skill_loader::load_all(&self.skills_dir) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"success": false, "error": format!("scan failed: {e}")})
                        .to_string(),
                });
            }
        };

        let mut candidates: Vec<&hermes_skill_loader::Skill> = skills
            .iter()
            .filter(|s| s.name == name || s.qualified_name == name)
            .collect();
        if candidates.is_empty() {
            let direct = self.skills_dir.join(name);
            if direct.is_dir() {
                let needle = direct.file_name().and_then(|s| s.to_str()).unwrap_or("");
                candidates = skills
                    .iter()
                    .filter(|s| {
                        s.source_path.parent() == Some(direct.as_path()) || s.name == needle
                    })
                    .collect();
            }
        }

        if candidates.is_empty() {
            let available: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
            return Ok(ToolOutput {
                content: json!({
                    "success": false,
                    "error": format!("Skill '{}' not found", name),
                    "available_skills": available,
                })
                .to_string(),
            });
        }
        if candidates.len() > 1 {
            let collisions: Vec<String> = candidates
                .iter()
                .map(|s| s.qualified_name.clone())
                .collect();
            return Ok(ToolOutput {
                content: json!({
                    "success": false,
                    "error": format!("Ambiguous skill name '{}': {} candidates", name, candidates.len()),
                    "candidates": collisions,
                })
                .to_string(),
            });
        }
        let skill = candidates.remove(0);
        let skill_root = skill
            .source_path
            .parent()
            .unwrap_or(&self.skills_dir)
            .to_path_buf();

        let linked = discover_linked_files(&skill_root);

        if let Some(fp) = file_path {
            return Ok(read_linked_file(skill, &skill_root, fp));
        }

        let raw = match std::fs::read(&skill.source_path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content:
                        json!({"success": false, "error": format!("read SKILL.md failed: {e}")})
                            .to_string(),
                });
            }
        };
        let text = String::from_utf8_lossy(&raw);
        let body = strip_frontmatter(&text);
        let truncated = crate::tools::bash::truncate_output(&body, 100_000);
        Ok(ToolOutput {
            content: json!({
                "success": true,
                "name": skill.name,
                "content": truncated,
                "description": skill.description,
                "linked_files": linked,
                "readiness_status": "available",
            })
            .to_string(),
        })
    }
}

fn read_linked_file(
    skill: &hermes_skill_loader::Skill,
    skill_root: &Path,
    file_path: &str,
) -> ToolOutput {
    if file_path.contains("..") {
        return ToolOutput {
            content: json!({
                "success": false,
                "error": format!("file_path escapes skill directory: {file_path}"),
            })
            .to_string(),
        };
    }

    let target = skill_root.join(file_path);
    let canon_root = match std::fs::canonicalize(skill_root) {
        Ok(p) => p,
        Err(e) => {
            return ToolOutput {
                content: json!({"success": false, "error": format!("canonicalize skill root failed: {e}")})
                    .to_string(),
            };
        }
    };
    let Ok(canon_target) = std::fs::canonicalize(&target) else {
        return ToolOutput {
            content: json!({
                "success": false,
                "error": format!("file_path not found: {file_path}"),
            })
            .to_string(),
        };
    };
    if !canon_target.starts_with(&canon_root) {
        return ToolOutput {
            content: json!({
                "success": false,
                "error": format!("file_path escapes skill directory: {file_path}"),
            })
            .to_string(),
        };
    }
    if !canon_target.is_file() {
        return ToolOutput {
            content: json!({
                "success": false,
                "error": format!("file_path is not a regular file: {file_path}"),
            })
            .to_string(),
        };
    }
    let raw = match std::fs::read(&canon_target) {
        Ok(b) => b,
        Err(e) => {
            return ToolOutput {
                content: json!({"success": false, "error": format!("read failed: {e}")})
                    .to_string(),
            };
        }
    };
    let content = String::from_utf8_lossy(&raw).into_owned();
    ToolOutput {
        content: json!({
            "success": true,
            "name": skill.name,
            "file": file_path,
            "content": content,
            "file_type": canon_target.extension().and_then(|s| s.to_str()).map(|s| format!(".{s}")),
            "description": skill.description,
        })
        .to_string(),
    }
}

fn strip_frontmatter(text: &str) -> String {
    let stripped = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !stripped.starts_with("---") {
        return stripped.to_string();
    }
    let after_open = match stripped.find('\n') {
        Some(i) => &stripped[i + 1..],
        None => return stripped.to_string(),
    };
    let mut idx = None;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            idx = Some(line.as_ptr() as usize - after_open.as_ptr() as usize);
            break;
        }
    }
    let Some(close) = idx else {
        return stripped.to_string();
    };
    after_open[close..]
        .trim_start_matches(['\n', '\r'])
        .trim_start_matches("---")
        .trim_start_matches('\n')
        .to_string()
}
