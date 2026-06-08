//! Skill data loading and system-prompt injection for the Perry Hermes agent.
//!
//! This crate is a leaf: it parses `SKILL.md` files, validates them, and renders
//! the prompt-injection metadata block. The LLM-callable runtime tools that
//! explore loaded skills (`SkillListTool`, `SkillViewTool`, ...) live in
//! `perry-hermes-agent::tools::skills`.
//!
//! See `docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md`
//! for the rename context.

pub mod frontmatter;
pub mod layout;
pub mod validate;

use std::path::PathBuf;

/// Render the system-prompt metadata index block for the given skills.
///
/// Returns `""` when `skills` is empty. The block groups categorized
/// skills first (alphabetical by category) and un-categorized skills
/// under a "general" heading. Within each group, skills sort by `name`.
///
/// The block intentionally references a `skill_view` tool that does not
/// exist yet; the LLM may fall back to reading SKILL.md via bash when
/// the `terminal` toolset is enabled. The actual `SkillViewTool`
/// is delivered in Phase 9's built-in tools expansion.
pub fn render_system_prompt_block(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut categorized: std::collections::BTreeMap<String, Vec<&Skill>> =
        std::collections::BTreeMap::new();
    let mut uncategorized: Vec<&Skill> = Vec::new();

    for s in skills {
        match &s.category {
            Some(c) => categorized.entry(c.clone()).or_default().push(s),
            None => uncategorized.push(s),
        }
    }

    let mut out = String::new();
    out.push_str(
        "The following skills are available. Each skill is a directory containing a SKILL.md file with detailed instructions.\n\
         Use the `skill_view` tool (or read the file directly with bash) to load a skill's body when it is relevant to the user's request.\n\n\
         Available skills:\n",
    );

    for (cat, mut members) in categorized {
        members.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str(&format!("\n**{cat}**:\n"));
        for s in members {
            out.push_str(&format!("- **{}**: {}\n", s.name, s.description));
        }
    }

    if !uncategorized.is_empty() {
        uncategorized.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str("\n**general** (uncategorized):\n");
        for s in uncategorized {
            out.push_str(&format!("- **{}**: {}\n", s.name, s.description));
        }
    }

    out
}

/// A single skill, loaded from a `SKILL.md` file with valid frontmatter.
///
/// The `frontmatter` field preserves every key from the YAML header
/// (including ones the runtime does not interpret, like `version` or
/// `platforms`) so future phases can read them without re-parsing.
#[derive(Debug, Clone)]
pub struct Skill {
    /// frontmatter.name, must equal the directory's basename.
    pub name: String,
    /// `category.name`, or just `name` when category is None.
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

use std::path::Path;

/// Scan `skills_dir` for valid skills.
///
/// - Returns `Ok(vec![])` when `skills_dir` does not exist.
/// - Returns `Err` when `read_dir` itself fails (permissions, vanished).
/// - Per-file problems (bad YAML, missing fields, validation failures,
///   mismatched directory names) are logged via `tracing::warn!` and the
///   file is skipped.
pub fn load_all(skills_dir: &Path) -> anyhow::Result<Vec<Skill>> {
    if !skills_dir.exists() {
        return Ok(Vec::new());
    }
    let locations = layout::find_skill_files(skills_dir)?;

    // Deduplicate by qualified_name within the same category.
    // First-seen wins.
    let mut seen: std::collections::HashMap<(Option<String>, String), ()> =
        std::collections::HashMap::new();
    let mut skills: Vec<Skill> = Vec::new();

    for loc in locations {
        let key = (loc.category.clone(), derive_name_key(&loc));
        if seen.contains_key(&key) {
            tracing::warn!(
                "skipping duplicate skill at {} (already loaded under {:?})",
                loc.skill_md.display(),
                key
            );
            continue;
        }
        if let Some(s) = parse_one(&loc) {
            seen.insert(key, ());
            skills.push(s);
        }
        // None branch: parse_one already logged a warn.
    }

    skills.sort_by(|a, b| {
        // Categorized first (Some(_)), then uncat (None).
        // Within each group, alphabetical by qualified_name.
        match (&a.category, &b.category) {
            (Some(x), Some(y)) => x.cmp(y).then(a.name.cmp(&b.name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }
    });

    Ok(skills)
}

fn derive_name_key(loc: &layout::SkillLocation) -> String {
    // Used only as a dedup key — the real `name` field comes from
    // frontmatter. We use the basename of the SKILL.md's parent.
    loc.skill_md
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parse_one(loc: &layout::SkillLocation) -> Option<Skill> {
    let raw = match std::fs::read_to_string(&loc.skill_md) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("skipping {}: failed to read: {e}", loc.skill_md.display());
            return None;
        }
    };

    let Some((fm, body)) = frontmatter::parse(&raw) else {
        tracing::warn!("skipping {}: no valid frontmatter", loc.skill_md.display());
        return None;
    };

    if !fm.is_mapping() {
        tracing::warn!(
            "skipping {}: frontmatter is not a YAML mapping",
            loc.skill_md.display()
        );
        return None;
    }

    let Some(name) = fm.get("name").and_then(|v| v.as_str()) else {
        tracing::warn!(
            "skipping {}: frontmatter missing `name`",
            loc.skill_md.display()
        );
        return None;
    };
    let Some(description) = fm.get("description").and_then(|v| v.as_str()) else {
        tracing::warn!(
            "skipping {}: frontmatter missing `description`",
            loc.skill_md.display()
        );
        return None;
    };

    if !validate::is_valid_name(name) {
        tracing::warn!(
            "skipping {}: invalid `name` {:?}",
            loc.skill_md.display(),
            name
        );
        return None;
    }
    if !validate::is_valid_description(description) {
        tracing::warn!(
            "skipping {}: invalid `description` {:?}",
            loc.skill_md.display(),
            description
        );
        return None;
    }
    if let Some(cat) = &loc.category
        && !validate::is_valid_category(cat)
    {
        tracing::warn!(
            "skipping {}: invalid category {:?} (skipping entire subtree)",
            loc.skill_md.display(),
            cat
        );
        return None;
    }

    // Directory basename must equal `name` byte-for-byte.
    let dir_basename = loc
        .skill_md
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir_basename != name {
        tracing::warn!(
            "skipping {}: directory name {:?} does not match frontmatter `name` {:?}",
            loc.skill_md.display(),
            dir_basename,
            name
        );
        return None;
    }

    let qualified_name = match &loc.category {
        Some(cat) => format!("{cat}.{name}"),
        None => name.to_string(),
    };

    Some(Skill {
        name: name.to_string(),
        qualified_name,
        category: loc.category.clone(),
        description: description.to_string(),
        body,
        frontmatter: fm,
        source_path: loc.skill_md.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_skill(root: &std::path::Path, rel: &str, contents: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, contents).unwrap();
    }

    const VALID_FM: &str = "---\nname: {NAME}\ndescription: \"{DESC}\"\n---\nbody\n";

    fn fm(name: &str, desc: &str) -> String {
        VALID_FM.replace("{NAME}", name).replace("{DESC}", desc)
    }

    #[test]
    fn loads_top_level_skill_with_dir_name_matching_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "rust-core-style/SKILL.md",
            &fm("rust-core-style", "Rust style guide"),
        );
        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.name, "rust-core-style");
        assert_eq!(s.qualified_name, "rust-core-style");
        assert_eq!(s.category, None);
        assert_eq!(s.description, "Rust style guide");
        assert_eq!(s.body, "body\n");
    }

    #[test]
    fn loads_one_level_nested_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "software-engineering/dogfood/SKILL.md",
            &fm("dogfood", "QA workflow"),
        );
        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.qualified_name, "software-engineering.dogfood");
        assert_eq!(s.category.as_deref(), Some("software-engineering"));
    }

    #[test]
    fn skips_various_invalid_skills_via_warn_and_skip() {
        // Each case exercises a different skip path in parse_one /
        // layout::find_skill_files. The point of grouping them is to
        // assert that `load_all` is best-effort: any single bad skill
        // is dropped, the rest are loaded, and no error is returned.
        let tmp = tempfile::tempdir().unwrap();
        // dir name doesn't match frontmatter name
        write_skill(tmp.path(), "weird-dir/SKILL.md", &fm("not-weird-dir", "x"));
        // invalid name (underscore)
        write_skill(tmp.path(), "Has_Upper/SKILL.md", &fm("Has_Upper", "x"));
        // invalid description (empty)
        write_skill(tmp.path(), "good-name/SKILL.md", &fm("good-name", ""));
        // missing frontmatter
        write_skill(tmp.path(), "no-fm/SKILL.md", "# Just markdown\n");
        // frontmatter missing required field
        write_skill(
            tmp.path(),
            "no-desc/SKILL.md",
            "---\nname: no-desc\n---\nbody\n",
        );
        // one good skill alongside
        write_skill(tmp.path(), "ok/SKILL.md", &fm("ok", "fine"));

        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1, "only the good skill survives");
        assert_eq!(skills[0].name, "ok");
    }

    #[test]
    fn allows_same_name_in_different_categories() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "cat-a/foo/SKILL.md", &fm("foo", "in cat a"));
        write_skill(tmp.path(), "cat-b/foo/SKILL.md", &fm("foo", "in cat b"));
        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 2);
        let mut qns: Vec<_> = skills.iter().map(|s| s.qualified_name.clone()).collect();
        qns.sort();
        assert_eq!(qns, vec!["cat-a.foo".to_string(), "cat-b.foo".to_string()]);
    }

    #[test]
    fn skips_duplicate_qualified_name_within_same_category() {
        let tmp = tempfile::tempdir().unwrap();
        // Two different top-level dirs with the same frontmatter name —
        // both have qualified_name == "foo", so the second is dropped.
        write_skill(tmp.path(), "foo/SKILL.md", &fm("foo", "first"));
        write_skill(tmp.path(), "bar/SKILL.md", &fm("foo", "second"));
        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "first");
    }

    #[test]
    fn returns_empty_vec_when_skills_dir_does_not_exist() {
        let p = std::path::Path::new("/definitely/does/not/exist/perry-hermes-skills-test");
        let skills = load_all(p).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn returns_empty_body_when_only_frontmatter_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "header-only/SKILL.md",
            "---\nname: header-only\ndescription: \"x\"\n---\n",
        );
        let skills = load_all(tmp.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body, "");
    }

    #[test]
    fn skills_are_sorted_by_category_then_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "zeta/SKILL.md", &fm("zeta", "z"));
        write_skill(tmp.path(), "alpha/SKILL.md", &fm("alpha", "a"));
        write_skill(tmp.path(), "cat-b/beta/SKILL.md", &fm("beta", "b in cat-b"));
        write_skill(
            tmp.path(),
            "cat-a/gamma/SKILL.md",
            &fm("gamma", "g in cat-a"),
        );
        let skills = load_all(tmp.path()).unwrap();
        let qns: Vec<_> = skills.iter().map(|s| s.qualified_name.as_str()).collect();
        // Categorized first (cat-a < cat-b), then uncat; within each group, alpha order.
        assert_eq!(qns, vec!["cat-a.gamma", "cat-b.beta", "alpha", "zeta"]);
    }

    #[test]
    fn read_dir_failure_returns_err() {
        // On Unix, `chmod 000` on a directory makes read_dir fail. Skip
        // on non-Unix platforms to keep the suite portable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let locked = tmp.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let result = load_all(&locked);
            // Restore so tempdir cleanup works.
            let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
            assert!(result.is_err(), "read_dir on chmod 000 should return Err");
        }
    }

    fn make_skill(name: &str, category: Option<&str>, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            qualified_name: match category {
                Some(c) => format!("{c}.{name}"),
                None => name.to_string(),
            },
            category: category.map(|s| s.to_string()),
            description: description.to_string(),
            body: String::new(),
            frontmatter: serde_yaml::Value::Null,
            source_path: Default::default(),
        }
    }

    #[test]
    fn render_empty_block_for_empty_vec() {
        assert_eq!(render_system_prompt_block(&[]), "");
    }

    #[test]
    fn render_block_groups_categorized_first_alphabetically() {
        let skills = vec![
            make_skill("alpha", None, "a"),
            make_skill("rust-style", Some("cat-a"), "r"),
            make_skill("python-style", Some("cat-a"), "p"),
            make_skill("beta", Some("cat-b"), "b"),
        ];
        let block = render_system_prompt_block(&skills);
        // Categorized first, alphabetical category order.
        let cat_a_idx = block.find("**cat-a**").expect("cat-a header present");
        let cat_b_idx = block.find("**cat-b**").expect("cat-b header present");
        let general_idx = block.find("**general**").expect("general header present");
        assert!(cat_a_idx < cat_b_idx);
        assert!(cat_b_idx < general_idx);
    }

    #[test]
    fn render_block_sorts_within_category_alphabetically() {
        let skills = vec![
            make_skill("zeta", Some("cat"), "z"),
            make_skill("alpha", Some("cat"), "a"),
        ];
        let block = render_system_prompt_block(&skills);
        let alpha_idx = block.find("- **alpha**").expect("alpha bullet present");
        let zeta_idx = block.find("- **zeta**").expect("zeta bullet present");
        assert!(alpha_idx < zeta_idx);
    }

    #[test]
    fn render_block_omits_general_section_when_all_categorized() {
        let skills = vec![make_skill("foo", Some("cat"), "f")];
        let block = render_system_prompt_block(&skills);
        assert!(!block.contains("**general**"));
    }

    #[test]
    fn render_block_uses_bold_name_and_colon_description() {
        let skills = vec![make_skill("foo", None, "the foo skill")];
        let block = render_system_prompt_block(&skills);
        assert!(block.contains("- **foo**: the foo skill"));
    }

    #[test]
    fn render_block_includes_forward_reference_to_skill_view_tool() {
        let skills = vec![make_skill("foo", None, "x")];
        let block = render_system_prompt_block(&skills);
        assert!(
            block.contains("skill_view"),
            "should reference skill_view tool for Phase 12"
        );
    }
}
