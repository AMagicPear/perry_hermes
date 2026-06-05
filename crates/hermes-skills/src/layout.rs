//! Walk a skills directory and identify valid skill files.

use std::path::{Path, PathBuf};

/// Names of directories that should be skipped wholesale during scan.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git", ".github", ".hub", ".archive", ".venv", "venv", "node_modules",
    "site-packages", "__pycache__", ".tox", ".nox", ".pytest_cache",
    ".mypy_cache", ".ruff_cache",
];

/// One discovered skill location, with the category derived from path depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLocation {
    pub skill_md: PathBuf,
    pub category: Option<String>,
}

/// Walk `skills_dir` and return the locations of every plausible
/// `SKILL.md` file, classified by depth. Excluded directories are
/// skipped silently. Files that don't fit the depth rules are
/// dropped silently here — the caller surfaces warnings when content
/// validation later fails.
///
/// Returns `Err` if `read_dir` on `skills_dir` itself fails (permissions,
/// vanished directory).
pub fn find_skill_files(skills_dir: &Path) -> Vec<SkillLocation> {
    let mut out = Vec::new();
    // Swallow errors: existing callers expect an empty vec on failure.
    let _ = walk_with_error(skills_dir, 0, None, &mut out);
    out
}

/// Variant of `find_skill_files` that propagates `read_dir` errors.
pub fn find_skill_files_with_error(
    skills_dir: &Path,
) -> std::io::Result<Vec<SkillLocation>> {
    let mut out = Vec::new();
    walk_with_error(skills_dir, 0, None, &mut out)?;
    Ok(out)
}

fn walk_with_error(
    dir: &Path,
    depth: usize,
    category: Option<&str>,
    out: &mut Vec<SkillLocation>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if EXCLUDED_DIRS.contains(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_file() {
            // Skip files — they are handled as children of directories.
            continue;
        }

        // We have a directory. Check if SKILL.md is a direct child file.
        let skill_md = path.join("SKILL.md");
        if skill_md.is_file() {
            // Skill at this depth with current category.
            out.push(SkillLocation {
                skill_md,
                category: category.map(String::from),
            });
        } else if depth == 0 {
            // Depth 0 dir with no SKILL.md: recurse as category container.
            // Establish this directory's name as the category.
            walk_with_error(&path, 1, Some(name), out)?;
        }
        // At depth >= 1, directories without direct SKILL.md are too deep —
        // skip (do not recurse).
    }
    Ok(())
}

#[allow(unused)]
fn walk(dir: &Path, depth: usize, category: Option<&str>, out: &mut Vec<SkillLocation>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if EXCLUDED_DIRS.contains(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_file() {
            // Skip files — they are handled as children of directories.
            continue;
        }

        // We have a directory. Check if SKILL.md is a direct child file.
        let skill_md = path.join("SKILL.md");
        if skill_md.is_file() {
            // Skill at this depth with current category.
            out.push(SkillLocation { skill_md, category: category.map(String::from) });
        } else if depth == 0 {
            // Depth 0 dir with no SKILL.md: recurse as category container.
            // Establish this directory's name as the category.
            walk(&path, 1, Some(name), out);
        }
        // At depth >= 1, directories without direct SKILL.md are too deep —
        // skip (do not recurse).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a skills tree from a `&[(&str, &str)]` list of
    /// `(relative_path, contents)` entries.
    fn build_tree(root: &Path, files: &[(&str, &str)]) {
        for (rel, contents) in files {
            let p = root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, contents).unwrap();
        }
    }

    fn loc_names(locs: &[SkillLocation]) -> Vec<(String, Option<String>)> {
        let mut v: Vec<_> = locs
            .iter()
            .map(|l| {
                let parent = l.skill_md.parent().unwrap();
                let dir_name = parent
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let category = l.category.clone();
                (dir_name, category)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn finds_top_level_skill() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(
            tmp.path(),
            &[("rust-core-style/SKILL.md", "---\nname: x\n---\n")],
        );
        let locs = find_skill_files(tmp.path());
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].category, None);
        assert_eq!(
            locs[0].skill_md.file_name().unwrap(),
            "SKILL.md"
        );
    }

    #[test]
    fn finds_one_level_nested_skill() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(
            tmp.path(),
            &[("software-engineering/dogfood/SKILL.md", "body")],
        );
        let locs = find_skill_files(tmp.path());
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].category.as_deref(), Some("software-engineering"));
    }

    #[test]
    fn ignores_two_level_nested() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(
            tmp.path(),
            &[("a/b/c/SKILL.md", "body")],
        );
        let locs = find_skill_files(tmp.path());
        assert!(locs.is_empty());
    }

    #[test]
    fn ignores_root_level_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(tmp.path(), &[("SKILL.md", "body")]);
        let locs = find_skill_files(tmp.path());
        assert!(locs.is_empty());
    }

    #[test]
    fn skips_excluded_directories() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(
            tmp.path(),
            &[
                ("real-skill/SKILL.md", "body"),
                (".git/inner-skill/SKILL.md", "body"),
                (".venv/inner-skill/SKILL.md", "body"),
                ("node_modules/inner-skill/SKILL.md", "body"),
            ],
        );
        let locs = find_skill_files(tmp.path());
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].category, None);
    }

    #[test]
    fn mixed_depths_returns_correct_set() {
        let tmp = tempfile::tempdir().unwrap();
        build_tree(
            tmp.path(),
            &[
                ("top/SKILL.md", "x"),
                ("cat1/inner/SKILL.md", "x"),
                ("cat1/other/SKILL.md", "x"),
                ("cat2/inner/SKILL.md", "x"),
                ("a/b/c/SKILL.md", "x"), // ignored
                ("SKILL.md", "x"),       // ignored
            ],
        );
        let locs = find_skill_files(tmp.path());
        let names = loc_names(&locs);
        assert_eq!(
            names,
            vec![
                ("inner".to_string(), Some("cat1".to_string())),
                ("inner".to_string(), Some("cat2".to_string())),
                ("other".to_string(), Some("cat1".to_string())),
                ("top".to_string(), None),
            ]
        );
    }
}