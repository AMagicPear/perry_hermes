//! `SkillListTool` / `SkillViewTool` — list installed skills and load their
//! bodies. Public contract (name, schema, toolset) is aligned with Python's
//! `skills_list` / `skill_view` so prompts and tool calls stay portable.

use std::path::PathBuf;

use async_trait::async_trait;
use hermes_core::error::ToolError;
use hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SKILLS_LIST_DESCRIPTION: &str = "List available skills (name + description). Use skill_view(name) to load full content.";
const SKILL_VIEW_DESCRIPTION: &str = "Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files (references, templates, scripts). First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path parameter.";

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
    fn name(&self) -> &str { "skills_list" }
    fn description(&self) -> &str { SKILLS_LIST_DESCRIPTION }
    fn toolset(&self) -> &'static str { "skills" }

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

        // Ensure the directory exists (spec: create on first access).
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

        let mut categories: Vec<String> = skills
            .iter()
            .filter_map(|s| s.category.clone())
            .collect();
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
    fn name(&self) -> &str { "skill_view" }
    fn description(&self) -> &str { SKILL_VIEW_DESCRIPTION }
    fn toolset(&self) -> &'static str { "skills" }

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

        let skills = match hermes_skills::load_all(&self.skills_dir) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"success": false, "error": format!("scan failed: {e}")})
                        .to_string(),
                });
            }
        };

        // Match by name, allowing qualified "category/name" form.
        let mut candidates: Vec<&hermes_skills::Skill> = skills
            .iter()
            .filter(|s| s.name == name || s.qualified_name == name)
            .collect();
        // Also try direct path match (e.g. "category/skill-name").
        if candidates.is_empty() {
            let direct = self.skills_dir.join(name);
            if direct.is_dir() {
                let needle = direct.file_name().and_then(|s| s.to_str()).unwrap_or("");
                candidates = skills
                    .iter()
                    .filter(|s| s.source_path.parent() == Some(direct.as_path()) || s.name == needle)
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
            let collisions: Vec<String> = candidates.iter().map(|s| s.qualified_name.clone()).collect();
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
        let skill_root = skill.source_path.parent().unwrap_or(&self.skills_dir).to_path_buf();

        // Discover linked files in references/ templates/ assets/ scripts/ (and "other").
        let linked = discover_linked_files(&skill_root);

        if let Some(fp) = file_path {
            // Reject path traversal.
            if fp.contains("..") {
                return Ok(ToolOutput {
                    content: json!({
                        "success": false,
                        "error": format!("file_path escapes skill directory: {fp}"),
                    })
                    .to_string(),
                });
            }
            let target = skill_root.join(fp);
            // Canonicalize-check that target is under skill_root.
            let canon_root = match std::fs::canonicalize(&skill_root) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ToolOutput {
                        content: json!({"success": false, "error": format!("canonicalize skill root failed: {e}")}).to_string(),
                    });
                }
            };
            let canon_target = match std::fs::canonicalize(&target) {
                Ok(p) => p,
                Err(_) => {
                    return Ok(ToolOutput {
                        content: json!({
                            "success": false,
                            "error": format!("file_path not found: {fp}"),
                        })
                        .to_string(),
                    });
                }
            };
            if !canon_target.starts_with(&canon_root) {
                return Ok(ToolOutput {
                    content: json!({
                        "success": false,
                        "error": format!("file_path escapes skill directory: {fp}"),
                    })
                    .to_string(),
                });
            }
            if !canon_target.is_file() {
                return Ok(ToolOutput {
                    content: json!({
                        "success": false,
                        "error": format!("file_path is not a regular file: {fp}"),
                    })
                    .to_string(),
                });
            }
            let raw = match std::fs::read(&canon_target) {
                Ok(b) => b,
                Err(e) => {
                    return Ok(ToolOutput {
                        content: json!({"success": false, "error": format!("read failed: {e}")}).to_string(),
                    });
                }
            };
            if looks_binary(&raw) {
                return Ok(ToolOutput {
                    content: json!({
                        "success": false,
                        "error": "Linked file is binary; not supported in this build.",
                    })
                    .to_string(),
                });
            }
            let content = String::from_utf8_lossy(&raw).into_owned();
            return Ok(ToolOutput {
                content: json!({
                    "success": true,
                    "name": skill.name,
                    "file": fp,
                    "content": content,
                    "file_type": canon_target.extension().and_then(|s| s.to_str()).map(|s| format!(".{s}")),
                    "description": skill.description,
                })
                .to_string(),
            });
        }

        // No file_path: read SKILL.md, strip frontmatter, return body.
        let raw = match std::fs::read(&skill.source_path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content: json!({"success": false, "error": format!("read SKILL.md failed: {e}")})
                        .to_string(),
                });
            }
        };
        let text = String::from_utf8_lossy(&raw);
        let body = strip_frontmatter(&text);
        // char-truncate to 100K head+tail.
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

fn strip_frontmatter(text: &str) -> String {
    // YAML frontmatter: starts with "---\n", ends with "\n---" on its own line.
    let stripped = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !stripped.starts_with("---") {
        return stripped.to_string();
    }
    let after_open = match stripped.find('\n') {
        Some(i) => &stripped[i + 1..],
        None => return stripped.to_string(),
    };
    // Find closing "\n---" at line start.
    let mut idx = None;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            idx = Some(line.as_ptr() as usize - after_open.as_ptr() as usize);
            break;
        }
    }
    let close = match idx {
        Some(o) => o,
        None => return stripped.to_string(),
    };
    after_open[close..]
        .trim_start_matches(['\n', '\r'])
        .trim_start_matches("---")
        .trim_start_matches('\n')
        .to_string()
}

fn discover_linked_files(skill_root: &std::path::Path) -> Value {
    let mut out = serde_json::Map::new();
    for bucket in ["references", "templates", "assets", "scripts"] {
        let dir = skill_root.join(bucket);
        let names = collect_bucket_files(skill_root, &dir, bucket_specific_extensions(bucket));
        if !names.is_empty() {
            out.insert(bucket.to_string(), Value::Array(names.into_iter().map(Value::String).collect()));
        }
    }
    let other: Vec<String> = match std::fs::read_dir(skill_root) {
        Ok(e) => e
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && n != "SKILL.md")
            .filter(|n| !["references", "templates", "assets", "scripts"].iter().any(|b| b == n))
            .collect(),
        Err(_) => Vec::new(),
    };
    if !other.is_empty() {
        out.insert("other".to_string(), Value::Array(other.into_iter().map(Value::String).collect()));
    }
    Value::Object(out)
}

fn bucket_specific_extensions(bucket: &str) -> Option<&'static [&'static str]> {
    match bucket {
        "references" => Some(&["md"]),
        "templates" => Some(&["md", "py", "yaml", "yml", "json", "tex", "sh"]),
        "scripts" => Some(&["py", "sh", "bash", "js", "ts", "rb"]),
        "assets" => None,
        _ => None,
    }
}

fn collect_bucket_files(
    skill_root: &std::path::Path,
    dir: &std::path::Path,
    allowed_extensions: Option<&[&str]>,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name_hidden = entry.file_name().to_string_lossy().starts_with('.');
        if name_hidden {
            continue;
        }
        if path.is_dir() {
            files.extend(collect_bucket_files(skill_root, &path, allowed_extensions));
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Some(exts) = allowed_extensions {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or_default();
            if !exts.contains(&ext) {
                continue;
            }
        }
        if let Ok(rel) = path.strip_prefix(skill_root) {
            files.push(rel.to_string_lossy().into_owned());
        }
    }
    files.sort();
    files
}

fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(1000)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|b| !matches!(**b, 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    (non_printable * 20) > sample.len()
}
