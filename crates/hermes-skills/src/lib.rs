//! Skill loading and system-prompt injection for the Hermes agent.
//!
//! See `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md`
//! for the full design.

pub mod frontmatter;
pub mod layout;
pub mod validate;

use std::path::PathBuf;

/// A single skill, loaded from a `SKILL.md` file with valid frontmatter.
///
/// The `frontmatter` field preserves every key from the YAML header
/// (including ones the runtime does not interpret, like `version` or
/// `platforms`) so future phases can read them without re-parsing.
#[derive(Debug, Clone)]
pub struct Skill {
    /// frontmatter.name, must equal the directory's basename.
    pub name: String,
    /// "<category>.<name>", or just <name> when category is None.
    pub qualified_name: String,
    /// Some("software-engineering") or None.
    pub category: Option<String>,
    /// frontmatter.description (already validated).
    pub description: String,
    /// Markdown body with the frontmatter block stripped.
    pub body: String,
    /// Full frontmatter as YAML value (other fields preserved).
    pub frontmatter: serde_yaml::Value,
    /// Absolute path of the SKILL.md file.
    pub source_path: PathBuf,
}