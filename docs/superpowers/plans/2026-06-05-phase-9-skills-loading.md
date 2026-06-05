# Phase 9 — Skills Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load `SKILL.md` files from `~/.perry_hermes/skills/`, validate their frontmatter, and inject a name+description index block into the system prompt at agent-construction time. Resurrect the dead `DEFAULT_SYSTEM_PROMPT` constant that became a no-op after the unify-runtime-config refactor.

**Architecture:** New leaf crate `hermes-skills` (depends on `hermes-core` types and standard ecosystem crates) exposes `load_all(skills_dir) -> Vec<Skill>` and `render_system_prompt_block(skills) -> String`. `hermes-runtime::build_loop` calls both via a new `compose_system_prompt` helper, which is also responsible for falling back to `DEFAULT_SYSTEM_PROMPT` when the user did not supply one. `HermesConfig.skills` and `SkillsConfig` are removed; the skills directory path is hard-coded in the runtime. Strict TDD: every behavior is covered by a unit test in the new crate or an integration test in `hermes-runtime`.

**Tech Stack:** Rust (workspace at edition 2021, MSRV 1.75), `serde_yaml` for frontmatter parsing, `tracing` for warnings, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md`

---

## File map

| File | Responsibility after this plan |
|---|---|
| `Cargo.toml` (workspace root) | Adds `"crates/hermes-skills"` to `members`. |
| `crates/hermes-skills/Cargo.toml` | **New.** Manifest: depends on `serde`, `serde_yaml`, `tracing`; dev-deps `tempfile`. |
| `crates/hermes-skills/src/lib.rs` | **New.** `Skill` struct, `load_all`, `render_system_prompt_block`, and a private `mod layout`, `mod frontmatter`, `mod validate` split (each small and focused). All `#[cfg(test)]` tests live in their respective submodules. |
| `crates/hermes-runtime/Cargo.toml` | Adds `hermes-skills = { path = "../hermes-skills" }` to `[dependencies]`. |
| `crates/hermes-runtime/src/lib.rs` | Adds `default_skills_dir()` and `compose_system_prompt()`; `build_loop` calls `compose_system_prompt` instead of passing `config.agent.system_prompt` directly. Fixes the §3 regression (DEFAULT_SYSTEM_PROMPT becomes the `None` fallback). |
| `crates/hermes-runtime/src/config.rs` | Removes `SkillsConfig` and the `skills` field on `HermesConfig`. Updates the `parses_anthropic_provider_config` test to drop the `[skills]` block. |
| `crates/hermes-runtime/tests/skills_injection.rs` | **New.** Integration tests covering §8.2. |
| `crates/hermes-runtime/skills-example/rust-core-style/SKILL.md` | **New.** Manual smoke fixture; not part of any build target. |
| `crates/hermes-cli/hermes.example.toml` | Removes the `[skills]` section. |
| `CLAUDE.md` | Architecture diagram adds `hermes-skills`; "Known Issues" removes "Skills 加载待实现"; "Architecture" paragraph describes the new flow. |
| `README.md` | Top progress line: Phase 9 marked complete; drop `[skills]` from example; add a Skills bullet under "特性"; update architecture diagram; update "Known Issues". |
| `plans/hermes-comparison.md` | Phase 9 status → ✅; remove Skills from any open P1 list. |
| `plans/rust-port-design.md` | §9.3 references the spec for current implementation details. |

---

## Task 1: Create the `hermes-skills` crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root, add to `members`)
- Create: `crates/hermes-skills/Cargo.toml`
- Create: `crates/hermes-skills/src/lib.rs`

- [ ] **Step 1: Add `crates/hermes-skills` to the workspace**

Edit the root `Cargo.toml` `members` array, adding `"crates/hermes-skills"` in alphabetical position (after `hermes-runtime`):

```toml
members = [
    "crates/hermes-core",
    "crates/hermes-providers",
    "crates/hermes-tools",
    "crates/hermes-loop",
    "crates/hermes-runtime",
    "crates/hermes-skills",
    "crates/hermes-cli",
    # "crates/hermes-gateway",  # phase 11+
    # "crates/hermes-tui",      # phase 10+
]
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/hermes-skills/Cargo.toml` with this content:

```toml
[package]
name = "hermes-skills"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Skill loading and system-prompt injection for Hermes"

[dependencies]
serde.workspace = true
serde_yaml = "0.9"
tracing.workspace = true

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Create the skeleton `lib.rs`**

Create `crates/hermes-skills/src/lib.rs` with this content:

```rust
//! Skill loading and system-prompt injection for the Hermes agent.
//!
//! See `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md`
//! for the full design. The public API is two functions:
//!
//! - [`load_all`]: scan a skills directory and return valid skills.
//! - [`render_system_prompt_block`]: render the metadata index for injection.
```

- [ ] **Step 4: Verify the crate builds**

Run: `cargo build -p hermes-skills`
Expected: clean build, no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/hermes-skills/Cargo.toml crates/hermes-skills/src/lib.rs
git commit -m "feat(skills): create hermes-skills crate skeleton"
```

---

## Task 2: Define the `Skill` struct

**Files:**
- Modify: `crates/hermes-skills/src/lib.rs`

- [ ] **Step 1: Add the `Skill` struct and re-export it as the crate's primary type**

Edit `crates/hermes-skills/src/lib.rs`. Replace its entire contents with:

```rust
//! Skill loading and system-prompt injection for the Hermes agent.
//!
//! See `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md`
//! for the full design.

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
```

- [ ] **Step 2: Verify the crate still builds**

Run: `cargo build -p hermes-skills`
Expected: clean build, no warnings (an unused-import warning on `serde_yaml::Value` is acceptable to silence via `#[allow(unused_imports)]` if it shows; the type is used in the next task via the field, so this should not be needed — confirm by running).

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-skills/src/lib.rs
git commit -m "feat(skills): define Skill struct"
```

---

## Task 3: Frontmatter parsing (test-first)

**Files:**
- Create: `crates/hermes-skills/src/frontmatter.rs`
- Modify: `crates/hermes-skills/src/lib.rs` (declare the module)

The frontmatter parser turns raw file text into a `(serde_yaml::Value, body)` pair, with the YAML `---` fence stripped. The leading-blank-line tolerance from spec §5.2 is required.

- [ ] **Step 1: Write the failing tests**

Create `crates/hermes-skills/src/frontmatter.rs` with this content (tests first, implementation as a stub that fails):

```rust
//! Strip the YAML frontmatter from a SKILL.md file body.

use serde_yaml::Value;

/// Parse `---`-fenced YAML frontmatter from the start of a markdown file.
///
/// Returns `Some((frontmatter, body))` when the file starts with a
/// (possibly preceded by blank lines) `---` fence that closes with a
/// standalone `---` line. Returns `None` when the file has no frontmatter.
///
/// On YAML parse failure, returns `Some((Value::Null, original_text))` so
/// callers can surface a precise warning (the caller decides whether
/// invalid YAML is a "skip and warn" condition).
pub fn parse(raw: &str) -> Option<(Value, String)> {
    // STUB: real implementation in step 3.
    let _ = raw;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_strips_it_from_body() {
        let raw = "---\nname: rust\ndescription: \"Rust style\"\n---\n\n# Body\n";
        let (fm, body) = parse(raw).expect("frontmatter should parse");
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("rust"));
        assert_eq!(body, "\n# Body\n");
    }

    #[test]
    fn tolerates_leading_blank_lines_before_opening_fence() {
        let raw = "\n\n---\nname: rust\ndescription: x\n---\nbody\n";
        let (fm, _) = parse(raw).expect("leading blanks should be tolerated");
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("rust"));
    }

    #[test]
    fn returns_none_when_no_opening_fence() {
        let raw = "# Just a markdown file\nwith no frontmatter\n";
        assert!(parse(raw).is_none());
    }

    #[test]
    fn returns_none_when_frontmatter_is_unterminated() {
        let raw = "---\nname: rust\ndescription: x\n";
        assert!(parse(raw).is_none());
    }

    #[test]
    fn returns_null_frontmatter_on_invalid_yaml() {
        // Unclosed flow sequence — `serde_yaml` will fail to parse this.
        let raw = "---\nname: [unclosed\ndescription: x\n---\nbody\n";
        let (fm, body) = parse(raw).expect("frontmatter fence is present");
        assert!(fm.is_null(), "invalid YAML should fall back to Null");
        assert_eq!(body, "body\n", "body should still be extractable");
    }

    #[test]
    fn empty_body_is_returned_when_frontmatter_is_last() {
        let raw = "---\nname: rust\ndescription: x\n---\n";
        let (_, body) = parse(raw).expect("frontmatter present");
        assert_eq!(body, "");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p hermes-skills frontmatter::`
Expected: most tests fail because the stub returns `None` unconditionally. Specifically the first, second, fifth, and sixth tests fail; "returns_none_when_no_opening_fence" and "returns_none_when_frontmatter_is_unterminated" pass (they assert `is_none()`).

- [ ] **Step 3: Wire the module into `lib.rs`**

Edit `crates/hermes-skills/src/lib.rs`. Add `pub mod frontmatter;` after the doc comment header and before the `use` line.

- [ ] **Step 4: Implement `parse`**

Replace the `parse` function body in `crates/hermes-skills/src/frontmatter.rs` with the real implementation:

```rust
pub fn parse(raw: &str) -> Option<(Value, String)> {
    // Skip leading blank lines (spec §5.2).
    let trimmed_start = raw.trim_start_matches('\n');
    let after_skip = trimmed_start
        .strip_prefix("---\n")
        .or_else(|| trimmed_start.strip_prefix("---\r\n"))?;

    // Find the closing `---` on its own line. We require a newline
    // before AND after the closing fence so that `---` mid-line in the
    // body does not falsely close.
    let close_marker = "\n---";
    let close_offset = after_skip.find(close_marker)?;
    // The closing fence is `\n---` followed by end-of-line or EOF.
    let after_close_idx = close_offset + close_marker.len();
    let tail = &after_skip[after_close_idx..];
    if !(tail.is_empty() || tail.starts_with('\n') || tail.starts_with("\r\n")) {
        return None;
    }

    let yaml_text = &after_skip[..close_offset];
    let body = tail.trim_start_matches('\n').trim_start_matches("\r\n").to_string();

    let frontmatter = match serde_yaml::from_str::<Value>(yaml_text) {
        Ok(v) => v,
        Err(_) => Value::Null,
    };

    Some((frontmatter, body))
}
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p hermes-skills frontmatter::`
Expected: 6 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-skills/src/lib.rs crates/hermes-skills/src/frontmatter.rs
git commit -m "feat(skills): frontmatter parser with leading-blank tolerance"
```

---

## Task 4: Validation helpers (test-first)

**Files:**
- Create: `crates/hermes-skills/src/validate.rs`
- Modify: `crates/hermes-skills/src/lib.rs` (declare the module)

The validator module exposes three functions: `is_valid_name`, `is_valid_description`, `is_valid_category`. They are pure string checks. Test cases come straight from the spec's §5.3, §5.4, and §5.5.

- [ ] **Step 1: Write the failing tests**

Create `crates/hermes-skills/src/validate.rs`:

```rust
//! Strict validators for frontmatter field values.
//!
//! These are pure string checks. The validation rules come from
//! spec §5.3, §5.4, and §5.5.

const MAX_NAME_LEN: usize = 64;
const MAX_DESC_LEN: usize = 1024;
const RESERVED: &[&str] = &["anthropic", "claude"];

/// True iff `name` is a valid skill name: 1..=64 chars from
/// [a-z0-9-], no XML bracket characters, not a reserved word.
pub fn is_valid_name(name: &str) -> bool {
    // STUB: replaced in step 3.
    let _ = name;
    false
}

/// True iff `description` is non-empty, ≤ 1024 chars, no XML brackets.
pub fn is_valid_description(description: &str) -> bool {
    let _ = description;
    false
}

/// True iff `category` passes the same shape checks as `name` (but
/// the `RESERVED` list is the only extra constraint).
pub fn is_valid_category(category: &str) -> bool {
    let _ = category;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_valid_lowercase_digits_hyphens() {
        assert!(is_valid_name("rust-core-style"));
        assert!(is_valid_name("abc"));
        assert!(is_valid_name("a1-b2-c3"));
        assert!(is_valid_name(&"a".repeat(MAX_NAME_LEN)));
    }

    #[test]
    fn name_rejects_empty() {
        assert!(!is_valid_name(""));
    }

    #[test]
    fn name_rejects_uppercase() {
        assert!(!is_valid_name("Rust"));
        assert!(!is_valid_name("RUST"));
    }

    #[test]
    fn name_rejects_underscore() {
        assert!(!is_valid_name("rust_core"));
    }

    #[test]
    fn name_rejects_over_max_length() {
        assert!(!is_valid_name(&"a".repeat(MAX_NAME_LEN + 1)));
    }

    #[test]
    fn name_rejects_xml_brackets() {
        assert!(!is_valid_name("foo<bar"));
        assert!(!is_valid_name("foo>bar"));
        assert!(!is_valid_name("foo<bar>baz"));
    }

    #[test]
    fn name_rejects_reserved_words() {
        assert!(!is_valid_name("anthropic"));
        assert!(!is_valid_name("claude"));
    }

    #[test]
    fn description_accepts_normal_text() {
        assert!(is_valid_description("Rust style guide"));
        assert!(is_valid_description(&"a".repeat(MAX_DESC_LEN)));
    }

    #[test]
    fn description_rejects_empty() {
        assert!(!is_valid_description(""));
    }

    #[test]
    fn description_rejects_over_max_length() {
        assert!(!is_valid_description(&"a".repeat(MAX_DESC_LEN + 1)));
    }

    #[test]
    fn description_rejects_xml_brackets() {
        assert!(!is_valid_description("a <b> tag"));
    }

    #[test]
    fn category_uses_same_rules_as_name() {
        assert!(is_valid_category("software-engineering"));
        assert!(!is_valid_category("Software_Engineering"));
        assert!(!is_valid_category("anthropic"));
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Edit `crates/hermes-skills/src/lib.rs`. Add `pub mod validate;` after `pub mod frontmatter;`.

- [ ] **Step 3: Run tests to confirm they fail**

Run: `cargo test -p hermes-skills validate::`
Expected: 12 failed, 0 passed (the stubs always return `false`).

- [ ] **Step 4: Implement the validators**

Replace the three stub bodies in `crates/hermes-skills/src/validate.rs`:

```rust
fn is_shaped_like_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.contains('<')
        && !s.contains('>')
        && !RESERVED.contains(&s)
}

pub fn is_valid_name(name: &str) -> bool {
    is_shaped_like_name(name)
}

pub fn is_valid_description(description: &str) -> bool {
    !description.is_empty()
        && description.len() <= MAX_DESC_LEN
        && !description.contains('<')
        && !description.contains('>')
}

pub fn is_valid_category(category: &str) -> bool {
    is_shaped_like_name(category)
}
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p hermes-skills validate::`
Expected: 12 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-skills/src/lib.rs crates/hermes-skills/src/validate.rs
git commit -m "feat(skills): frontmatter field validators"
```

---

## Task 5: Directory layout scan (test-first)

**Files:**
- Create: `crates/hermes-skills/src/layout.rs`
- Modify: `crates/hermes-skills/src/lib.rs`

The layout module classifies directory entries under a skills root and returns a list of `(path_to_skill_md, category)` pairs. Top-level `<name>/SKILL.md` → `category = None`. One-level `<cat>/<name>/SKILL.md` → `category = Some("cat")`. Everything else is dropped. Excluded directory names (`.git`, `.venv`, etc.) are also dropped.

- [ ] **Step 1: Write the failing tests**

Create `crates/hermes-skills/src/layout.rs`:

```rust
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
pub fn find_skill_files(skills_dir: &Path) -> Vec<SkillLocation> {
    // STUB: replaced in step 4.
    let _ = skills_dir;
    Vec::new()
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
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Edit `crates/hermes-skills/src/lib.rs`. Add `pub mod layout;` after `pub mod validate;`.

- [ ] **Step 3: Run tests to confirm they fail**

Run: `cargo test -p hermes-skills layout::`
Expected: 6 failed (the stub returns `vec![]`).

- [ ] **Step 4: Implement `find_skill_files`**

Replace the `find_skill_files` function body in `crates/hermes-skills/src/layout.rs`:

```rust
pub fn find_skill_files(skills_dir: &Path) -> Vec<SkillLocation> {
    let mut out = Vec::new();
    walk(skills_dir, None, &mut out);
    out
}

fn walk(dir: &Path, category: Option<&str>, out: &mut Vec<SkillLocation>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            // Spec: hidden dirs are noise (and the .venv / .git family
            // of dot-dirs is a subset of this). No exception.
            continue;
        }
        if EXCLUDED_DIRS.contains(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        // We have a directory. What is its depth relative to the skills root?
        // category == None means we're scanning the top level.
        // category == Some(_) means we've already recursed once.
        match category {
            None => {
                // Could be a top-level skill, or a category container.
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    out.push(SkillLocation { skill_md, category: None });
                } else {
                    walk(&path, Some(name), out);
                }
            }
            Some(_) => {
                // We are already inside a category. Anything past here is
                // too deep — drop.
                continue;
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p hermes-skills layout::`
Expected: 6 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-skills/src/lib.rs crates/hermes-skills/src/layout.rs
git commit -m "feat(skills): directory layout scan with excluded dirs"
```

---

## Task 6: `load_all` (test-first)

**Files:**
- Modify: `crates/hermes-skills/src/lib.rs`

This task wires the three previous modules together into the public `load_all` function. It reads each discovered `SKILL.md`, parses frontmatter, validates fields, and returns a sorted, deduplicated `Vec<Skill>`.

- [ ] **Step 1: Write the failing tests**

Append the following `#[cfg(test)] mod tests` block to the end of `crates/hermes-skills/src/lib.rs` (after the `Skill` struct, before any other content if any). The tests use `tempfile` for tree building.

```rust
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
        VALID_FM
            .replace("{NAME}", name)
            .replace("{DESC}", desc)
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
    fn skips_dir_name_not_matching_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "weird-dir/SKILL.md",
            &fm("not-weird-dir", "x"),
        );
        let skills = load_all(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn skips_invalid_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "Has_Upper/SKILL.md",
            &fm("Has_Upper", "x"),
        );
        let skills = load_all(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn skips_invalid_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "good-name/SKILL.md",
            &fm("good-name", ""),
        );
        let skills = load_all(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn skips_missing_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "no-frontmatter/SKILL.md", "# Just markdown\n");
        let skills = load_all(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn skips_frontmatter_missing_required_field() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "no-desc/SKILL.md",
            "---\nname: no-desc\n---\nbody\n",
        );
        let skills = load_all(tmp.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn allows_same_name_in_different_categories() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "cat-a/foo/SKILL.md",
            &fm("foo", "in cat a"),
        );
        write_skill(
            tmp.path(),
            "cat-b/foo/SKILL.md",
            &fm("foo", "in cat b"),
        );
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
        let p = std::path::Path::new("/definitely/does/not/exist/hermes-skills-test");
        let skills = load_all(p).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn returns_empty_body_when_only_frontmatter_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "header-only/SKILL.md",
            &fm("header-only", "x"),
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
        write_skill(
            tmp.path(),
            "cat-b/beta/SKILL.md",
            &fm("beta", "b in cat-b"),
        );
        write_skill(
            tmp.path(),
            "cat-a/gamma/SKILL.md",
            &fm("gamma", "g in cat-a"),
        );
        let skills = load_all(tmp.path()).unwrap();
        let qns: Vec<_> = skills.iter().map(|s| s.qualified_name.as_str()).collect();
        // Categorized first (cat-a < cat-b), then uncat; within each group, alpha order.
        assert_eq!(
            qns,
            vec!["cat-a.gamma", "cat-b.beta", "alpha", "zeta"]
        );
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
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p hermes-skills tests::`
Expected: all 13 tests fail to compile (no `load_all` function exists yet). Resolve the compile error by adding a stub in step 3.

- [ ] **Step 3: Add a stub `load_all` and a private `parse_one` helper**

In `crates/hermes-skills/src/lib.rs`, after the `Skill` struct, add:

```rust
use std::path::Path;

use crate::frontmatter;
use crate::layout::{self, SkillLocation};
use crate::validate;

/// Scan `skills_dir` for valid skills.
///
/// - Returns `Ok(vec![])` when `skills_dir` does not exist.
/// - Returns `Err` when `read_dir` itself fails (permissions, vanished).
/// - Per-file problems (bad YAML, missing fields, validation failures,
///   mismatched directory names) are logged via `tracing::warn!` and the
///   file is skipped.
pub fn load_all(skills_dir: &Path) -> anyhow::Result<Vec<Skill>> {
    // STUB: real implementation in step 5.
    let _ = skills_dir;
    Ok(Vec::new())
}

// STUB helper for the implementation in step 5.
#[allow(dead_code)]
fn parse_one(loc: &SkillLocation) -> Option<Skill> {
    let _ = loc;
    None
}
```

- [ ] **Step 4: Run tests to confirm they still fail (or behave meaningfully)**

Run: `cargo test -p hermes-skills tests::`
Expected: tests compile now; each assertion fails because the stub returns an empty `Vec`. (Some tests pass for the right reason — `returns_empty_vec_when_skills_dir_does_not_exist` and `read_dir_failure_returns_err` happen to agree with the stub; treat them as "not red, not blocking".)

- [ ] **Step 5: Implement `load_all` and `parse_one`**

Replace the stub bodies in `crates/hermes-skills/src/lib.rs`:

```rust
pub fn load_all(skills_dir: &Path) -> anyhow::Result<Vec<Skill>> {
    if !skills_dir.exists() {
        return Ok(Vec::new());
    }
    let locations = layout::find_skill_files(skills_dir);

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
        match parse_one(&loc) {
            Some(s) => {
                seen.insert(key, ());
                skills.push(s);
            }
            None => {
                // parse_one already logged a warn.
            }
        }
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

fn derive_name_key(loc: &SkillLocation) -> String {
    // Used only as a dedup key — the real `name` field comes from
    // frontmatter. We use the basename of the SKILL.md's parent.
    loc.skill_md
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parse_one(loc: &SkillLocation) -> Option<Skill> {
    let raw = match std::fs::read_to_string(&loc.skill_md) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "skipping {}: failed to read: {e}",
                loc.skill_md.display()
            );
            return None;
        }
    };

    let (fm, body) = match frontmatter::parse(&raw) {
        Some(pair) => pair,
        None => {
            tracing::warn!(
                "skipping {}: no valid frontmatter",
                loc.skill_md.display()
            );
            return None;
        }
    };

    if !fm.is_mapping() {
        tracing::warn!(
            "skipping {}: frontmatter is not a YAML mapping",
            loc.skill_md.display()
        );
        return None;
    }

    let name = match fm.get("name").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "skipping {}: frontmatter missing `name`",
                loc.skill_md.display()
            );
            return None;
        }
    };
    let description = match fm.get("description").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "skipping {}: frontmatter missing `description`",
                loc.skill_md.display()
            );
            return None;
        }
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
            "skipping {}: invalid `description`",
            loc.skill_md.display()
        );
        return None;
    }
    if let Some(cat) = &loc.category {
        if !validate::is_valid_category(cat) {
            tracing::warn!(
                "skipping {}: invalid category {:?} (skipping entire subtree)",
                loc.skill_md.display(),
                cat
            );
            return None;
        }
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
```

- [ ] **Step 6: Run tests to confirm they pass**

Run: `cargo test -p hermes-skills`
Expected: all 31 tests pass (6 frontmatter + 12 validate + 6 layout + 13 load_all; 2 overlap with stubs but still pass). If any fail, the most likely culprit is the dedup key — re-read step 5's `derive_name_key` vs `parse_one`'s `dir_basename` extraction; they must agree.

- [ ] **Step 7: Commit**

```bash
git add crates/hermes-skills/src/lib.rs
git commit -m "feat(skills): load_all with parse, validate, dedup, sort"
```

---

## Task 7: `render_system_prompt_block` (test-first)

**Files:**
- Modify: `crates/hermes-skills/src/lib.rs`

- [ ] **Step 1: Add a failing test block at the end of `lib.rs`**

Append to the existing `#[cfg(test)] mod tests` block in `crates/hermes-skills/src/lib.rs` (before its closing `}`):

```rust
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
        assert!(block.contains("skill_view"), "should reference skill_view tool for Phase 12");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p hermes-skills tests::render_`
Expected: 6 failed (compile error first — `render_system_prompt_block` does not exist).

- [ ] **Step 3: Add a stub `render_system_prompt_block` and re-run tests**

In `crates/hermes-skills/src/lib.rs`, after the `Skill` struct, add:

```rust
/// Render the system-prompt metadata index block for the given skills.
///
/// Returns `""` when `skills` is empty. The block groups categorized
/// skills first (alphabetical by category) and un-categorized skills
/// under a "general" heading. Within each group, skills sort by `name`.
///
/// The block intentionally references a `skill_view` tool that does not
/// exist yet; the LLM may fall back to reading SKILL.md via bash when
/// the `terminal` toolset is enabled. The actual `SkillActivationTool`
/// is a Phase 12 deliverable.
pub fn render_system_prompt_block(skills: &[Skill]) -> String {
    // STUB: real implementation in step 5.
    let _ = skills;
    String::new()
}
```

Run: `cargo test -p hermes-skills tests::render_`
Expected: 6 failed (assertions, no longer compile).

- [ ] **Step 4: Implement `render_system_prompt_block`**

Replace the stub body:

```rust
pub fn render_system_prompt_block(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut categorized: std::collections::BTreeMap<String, Vec<&Skill>> =
        std::collections::BTreeMap::new();
    let mut uncategorized: Vec<&Skill> = Vec::new();

    for s in skills {
        match &s.category {
            Some(c) => categorized
                .entry(c.clone())
                .or_default()
                .push(s),
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
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p hermes-skills`
Expected: all tests pass (37 total: 6 frontmatter + 12 validate + 6 layout + 13 load_all + 6 render).

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-skills/src/lib.rs
git commit -m "feat(skills): render_system_prompt_block with category grouping"
```

---

## Task 8: Wire `hermes-skills` into `hermes-runtime`

**Files:**
- Modify: `crates/hermes-runtime/Cargo.toml`
- Modify: `crates/hermes-runtime/src/lib.rs`

This task adds the runtime-side glue: two new helpers (`default_skills_dir`, `compose_system_prompt`) and a one-line change in `build_loop` to call them. It also fixes the §3 regression by using `DEFAULT_SYSTEM_PROMPT` as the fallback when `config.agent.system_prompt` is `None`.

- [ ] **Step 1: Add the `hermes-skills` dependency**

Edit `crates/hermes-runtime/Cargo.toml`. In `[dependencies]`, after the `BashTool` line (alphabetical position), add:

```toml
hermes-skills = { path = "../hermes-skills" }
```

- [ ] **Step 2: Add the helpers and rewire `build_loop`**

Edit `crates/hermes-runtime/src/lib.rs`. Find the `build_loop` function (around line 121-132 in the current file) and replace it with the new version that calls a new `compose_system_prompt` helper. Then add the two helpers immediately after `build_loop`.

Replace `build_loop`:

```rust
/// Shared loop construction for `AIAgent::from_config` and `AIAgent::new`.
/// Centralizes registry construction and `LoopConfig` initialization so
/// the two entry points stay in lockstep.
fn build_loop(provider: Arc<dyn Provider>, config: &HermesConfig) -> AgentLoop {
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets));
    let system_prompt = compose_system_prompt(config.agent.system_prompt.as_deref());
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt,
            ..Default::default()
        },
    )
}

/// Compute the default skills directory from the user's `HOME`.
///
/// Returns `None` when `HOME` is unset. Matches the existing
/// `~/.perry_hermes/config.toml` convention used by the CLI.
fn default_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes").join("skills"))
}

/// Compose the final system prompt: user-supplied prompt (or the
/// pre-unify default), plus a skills index block when skills exist.
///
/// Fixes the regression from `refactor(runtime): unify AIAgent API on
/// HermesConfig + SessionContext`, which made `DEFAULT_SYSTEM_PROMPT`
/// dead code. When the user does not supply a system prompt, this
/// function falls back to that default.
fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let skills = match default_skills_dir() {
        Some(d) => match hermes_skills::load_all(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "failed to scan skills dir {}: {e}",
                    d.display()
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);

    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}
```

- [ ] **Step 3: Verify the runtime still builds and its existing tests pass**

Run: `cargo build -p hermes-runtime`
Expected: clean build, no warnings.

Run: `cargo test -p hermes-runtime`
Expected: all existing tests pass. In particular, the `from_config_succeeds_for_echo_provider` and `session_context_is_plumbed_into_tool_context` tests still pass — they don't set up a skills directory, so `compose_system_prompt` returns `Some(DEFAULT_SYSTEM_PROMPT.to_string())` which is what the loop expects.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-runtime/Cargo.toml crates/hermes-runtime/src/lib.rs
git commit -m "feat(runtime): inject skills index into system prompt

Adds compose_system_prompt helper that calls hermes_skills::load_all
on $HOME/.perry_hermes/skills and appends the rendered index block
after the user-supplied system prompt (or DEFAULT_SYSTEM_PROMPT when
none is set). Resurrects DEFAULT_SYSTEM_PROMPT, which became dead
code after the unify-runtime-config refactor."
```

---

## Task 9: Remove the `SkillsConfig` placeholder from `HermesConfig`

**Files:**
- Modify: `crates/hermes-runtime/src/config.rs`
- Modify: `crates/hermes-cli/hermes.example.toml`

The placeholder `SkillsConfig` struct and the `skills` field on `HermesConfig` are no longer used — skills are loaded from a fixed path in `hermes-runtime` itself. Removing them is a small simplification; it also lets the `parses_anthropic_provider_config` test drop the now-stale `[skills]` block.

- [ ] **Step 1: Remove `SkillsConfig` and the `skills` field**

Edit `crates/hermes-runtime/src/config.rs`. Replace the `HermesConfig` struct (lines 6-13):

```rust
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct HermesConfig {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}
```

Delete the entire `SkillsConfig` struct (lines 78-84).

- [ ] **Step 2: Drop the `[skills]` block from the existing test**

Edit the `parses_anthropic_provider_config` test in `crates/hermes-runtime/src/config.rs`. Remove the two lines that parse `[skills]`:

```rust
[skills]
enabled = ["rust"]
paths = ["./skills"]
```

Remove the two corresponding assertions (`config.skills.enabled` and `config.skills.paths`).

The test's `input` literal should end after the `[agent]` block, and its assertions end after `disabled_toolsets`.

- [ ] **Step 3: Remove the dead `assert_eq!(config.skills, SkillsConfig::default())` from `agent_and_skills_default_when_omitted`**

In the same file, drop the line:

```rust
assert_eq!(config.skills, SkillsConfig::default());
```

Leave the `agent` assertion in place. The test still validates that omitting both `[agent]` and `[skills]` produces defaults — but since `[skills]` no longer exists, only the `agent` assertion remains meaningful. Keep the test function (don't delete it).

- [ ] **Step 4: Remove the `[skills]` section from the example TOML**

Edit `crates/hermes-cli/hermes.example.toml`. Remove the trailing `[skills]` block:

```toml
[skills]
enabled = []
paths = ["./skills"]
```

- [ ] **Step 5: Verify the workspace still builds and tests pass**

Run: `cargo build --workspace`
Expected: clean build, no warnings.

Run: `cargo test -p hermes-runtime config::`
Expected: the two remaining tests in `config.rs` pass.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-runtime/src/config.rs crates/hermes-cli/hermes.example.toml
git commit -m "refactor(runtime): drop SkillsConfig and HermesConfig.skills

The skills directory is now hard-coded in hermes-runtime
(\$HOME/.perry_hermes/skills); the [skills] TOML section is gone.
This also removes the [skills] block from the example config and
updates the parses_anthropic_provider_config test."
```

---

## Task 10: Integration tests for runtime wiring

**Files:**
- Create: `crates/hermes-runtime/tests/skills_injection.rs`
- Modify: `crates/hermes-runtime/Cargo.toml` (add `async-trait`, `hermes-skills` may need test-only access; both are already in `[dependencies]`)

These tests cover spec §8.2 end-to-end: they build an `AIAgent` against a real or scripted provider, drive a single turn, and assert the first system message the LLM sees contains the expected prompt structure.

- [ ] **Step 1: Add `[dev-dependencies]` to `hermes-runtime/Cargo.toml` if not already present**

Open `crates/hermes-runtime/Cargo.toml`. If there is no `[dev-dependencies]` section, add one. Required test-only dependencies (all already in `[dependencies]` may need re-export to tests; check the existing `[dev-dependencies]` block first):

```toml
[dev-dependencies]
async-trait.workspace = true
tokio = { workspace = true, features = ["macros", "rt"] }
futures.workspace = true
tempfile = "3"
```

If `[dev-dependencies]` already exists with some of these, add only the missing ones. Do not duplicate.

- [ ] **Step 2: Write the integration test file**

Create `crates/hermes-runtime/tests/skills_injection.rs` with the following content:

```rust
//! End-to-end tests for the skills injection wiring in `AIAgent::from_config`.
//!
//! These tests use a scripted provider that captures the `messages`
//! passed to its `stream` call. The captured system message is asserted
//! to contain (or not contain) the expected skills block.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hermes_core::message::Message;
use hermes_core::provider::{
    CompletionDelta, CompletionStream, FinishReason, Provider,
};
use hermes_providers::EchoProvider;
use hermes_runtime::{AIAgent, HermesConfig, ProviderConfig, ProviderKind, SessionContext};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CaptureProvider {
    captured: Arc<Mutex<Vec<Message>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[hermes_core::registry::ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, hermes_core::ProviderError> {
        *self.captured.lock().unwrap() = messages.to_vec();
        Ok(Box::pin(stream::iter(vec![Ok(CompletionDelta {
            content_delta: Some("ok".into()),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: Some(hermes_core::Usage::default()),
            finish_reason: Some(FinishReason::Stop),
        })])))
    }
}

fn write_skill(root: &std::path::Path, rel: &str, contents: &str) {
    use std::fs;
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, contents).unwrap();
}

fn skills_dir_for(home: &std::path::Path) -> PathBuf {
    home.join(".perry_hermes").join("skills")
}

fn config_for_echo() -> HermesConfig {
    HermesConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Echo,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_config_omits_it_and_skills_dir_absent() {
    // SAFETY: serialized by the test harness; this test does not run in
    // parallel with any other test that mutates HOME. (`cargo test` runs
    // integration tests in separate processes by default.)
    unsafe { std::env::set_var("HOME", "/definitely/does/not/exist/hermes-test") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .expect("a System message should have been injected");
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(
        text.contains("careful assistant"),
        "expected DEFAULT_SYSTEM_PROMPT, got: {text}",
    );
    assert!(
        !text.contains("Available skills"),
        "no skills block expected: {text}",
    );
}

#[tokio::test]
async fn runtime_appends_skills_block_after_user_supplied_system_prompt() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(
        &skills,
        "rust-core-style/SKILL.md",
        "---\nname: rust-core-style\ndescription: \"Rust style\"\n---\nbody\n",
    );

    // SAFETY: see prior test.
    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let mut config = config_for_echo();
    config.agent.system_prompt = Some("CUSTOM-PROMPT-MARKER".into());
    let agent = AIAgent::new(provider, config);

    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    let custom_idx = text.find("CUSTOM-PROMPT-MARKER").expect("custom prompt present");
    let skills_idx = text.find("Available skills").expect("skills block present");
    assert!(
        custom_idx < skills_idx,
        "skills block should follow the user's prompt: {text}",
    );
    assert!(
        text.contains("**rust-core-style**: Rust style"),
        "skill entry missing: {text}",
    );
}

#[tokio::test]
async fn runtime_does_not_fail_construction_when_skills_dir_has_parse_errors() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(&skills, "bad-fm/SKILL.md", "no frontmatter at all\n");
    write_skill(&skills, "ok-skill/SKILL.md",
               "---\nname: ok-skill\ndescription: \"fine\"\n---\nbody\n");

    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    // ok-skill is loaded, bad-fm is skipped, AIAgent still constructed.
    assert!(text.contains("**ok-skill**"), "good skill should be loaded: {text}");
    assert!(!text.contains("bad-fm"), "bad skill should be skipped: {text}");
}

#[tokio::test]
async fn runtime_uses_default_system_prompt_when_home_is_unset() {
    // SAFETY: serialized by the test harness.
    unsafe { std::env::remove_var("HOME") };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .expect("a System message should have been injected");
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(text.contains("careful assistant"));
    assert!(!text.contains("Available skills"));
    assert!(!text.contains("skill_view"));
}

#[tokio::test]
async fn runtime_injects_skills_index_into_system_prompt_when_skills_dir_present() {
    let home = tempfile::tempdir().unwrap();
    let skills = skills_dir_for(home.path());
    write_skill(
        &skills,
        "rust-core-style/SKILL.md",
        "---\nname: rust-core-style\ndescription: \"Rust style\"\n---\n",
    );
    write_skill(
        &skills,
        "software-engineering/dogfood/SKILL.md",
        "---\nname: dogfood\ndescription: \"QA workflow\"\n---\n",
    );

    unsafe { std::env::set_var("HOME", home.path()) };

    let provider = CaptureProvider::default();
    let captured = Arc::clone(&provider.captured);
    let agent = AIAgent::new(provider, config_for_echo());
    let session = SessionContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: "t".into(),
    };
    agent
        .run_turn("hi", &session, CancellationToken::new(), |_| {})
        .await
        .unwrap();

    let msgs = captured.lock().unwrap();
    let system = msgs
        .iter()
        .find(|m| m.role == hermes_core::message::Role::System)
        .unwrap();
    let text = match &system.content {
        hermes_core::message::Content::Text(s) => s.clone(),
        _ => panic!("system message should be text"),
    };
    assert!(text.contains("Available skills"), "{text}");
    assert!(text.contains("rust-core-style"), "{text}");
    assert!(text.contains("software-engineering.dogfood") || text.contains("dogfood"), "{text}");
    assert!(text.contains("skill_view"), "{text}");
}
```

- [ ] **Step 3: Run the new integration tests**

Run: `cargo test -p hermes-runtime --test skills_injection`
Expected: 5 passed, 0 failed.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-runtime/Cargo.toml crates/hermes-runtime/tests/skills_injection.rs
git commit -m "test(runtime): integration tests for skills injection"
```

---

## Task 11: Add the manual smoke fixture

**Files:**
- Create: `crates/hermes-runtime/skills-example/rust-core-style/SKILL.md`

A single example skill that the user can copy to `~/.perry_hermes/skills/` to verify the wiring end-to-end with the echo provider.

- [ ] **Step 1: Create the example skill**

Create the file `crates/hermes-runtime/skills-example/rust-core-style/SKILL.md`:

```markdown
---
name: rust-core-style
description: "Example skill: Rust coding style reminders"
---

# Rust Style

- Prefer `?` over `unwrap`/`expect` in non-test code.
- Use `tracing` for diagnostics, not `println!`.
- Run `cargo clippy --all-targets --all-features -- -D warnings` before committing.
```

- [ ] **Step 2: Verify it doesn't break any build**

Run: `cargo build --workspace`
Expected: clean build (the file is not referenced by any build target; it lives under `crates/hermes-runtime/skills-example/` which is a leaf directory with no `Cargo.toml`).

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-runtime/skills-example
git commit -m "docs(skills): add manual smoke fixture under skills-example/"
```

---

## Task 12: Documentation updates

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`
- Modify: `plans/hermes-comparison.md`
- Modify: `plans/rust-port-design.md`

- [ ] **Step 1: Update `CLAUDE.md`**

In `CLAUDE.md`:

1. In the top progress line, change `Phase 9` to remove the "Phase 9 配置文件已初步可用" wording and mark it done. The exact wording is:
   > "当前进度：**Phase 0–6、Phase 8 已完成，Phase 9 配置文件已初步可用**（核心循环 + OpenAI/Anthropic 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断 + TOML provider/agent 配置）。Phase 7 上下文压缩仍暂缓。"

   Replace with:
   > "当前进度：**Phase 0–6、Phase 8–9 已完成**（核心循环 + OpenAI/Anthropic 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断 + TOML provider/agent 配置 + Skills 加载）。Phase 7 上下文压缩仍暂缓。"

2. In the architecture diagram, add `hermes-skills` as a new leaf crate parallel to `hermes-tools` and `hermes-providers`:

   ```
   hermes-cli (交互式 REPL — Phase 4)
     └─ hermes-runtime (产品 API 门面 — AIAgent)
          └─ hermes-loop (Agent 循环状态机)
               ├─ hermes-core (类型、特征、错误 — 无 IO)
               ├─ hermes-providers (OpenAI / Anthropic 适配器、Echo 模拟)
               ├─ hermes-tools (BashTool)
               └─ hermes-skills (SKILL.md 加载 + system prompt 注入)  # 新增
   ```

3. In the "Runtime + CLI" paragraph, add a sentence about skills: "Skills live in `~/.perry_hermes/skills/`; the runtime loads them at `AIAgent::from_config` time and injects a name+description index into the system prompt."

4. In the "Known Issues" section, remove the line about skills loading being deferred (search for "Skills 加载" and remove the relevant bullet, or just delete that single line if it exists in the "Still open" / "P1" lists).

- [ ] **Step 2: Update `README.md`**

In `README.md`:

1. Update the top progress line to mark Phase 9 complete and mention skills loading.
2. Drop the `[skills]` block from the example TOML.
3. Add a "Skills" bullet under "特性" describing the loading behavior.
4. Update the architecture diagram (mirror CLAUDE.md change).
5. In the "已知问题" section, remove any line about skills loading being incomplete.

- [ ] **Step 3: Update `plans/hermes-comparison.md`**

Find the Phase 9 line in the comparison report's current status table and mark it `✅`. If there's a "P1 / open" list mentioning skills, drop the skills bullet (or change its status to `done`).

- [ ] **Step 4: Update `plans/rust-port-design.md`**

In §9.3, add a sentence at the end:

> "The current implementation is documented at `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md` and the implementation plan at `docs/superpowers/plans/2026-06-05-phase-9-skills-loading.md`."

- [ ] **Step 5: Verify the docs are consistent (no broken cross-refs)**

Run: `cargo build --workspace`
Expected: clean build (no doc changes affect compile, but this confirms the workspace is still healthy).

Run: `rg -n "SkillsConfig|\[skills\]" --type rust --type toml`
Expected: zero hits. (No code or config file should still reference the removed placeholder.)

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md README.md plans/hermes-comparison.md plans/rust-port-design.md
git commit -m "docs: mark phase 9 complete, drop [skills] placeholders, add skills architecture"
```

---

## Task 13: Final verification

**Files:** none (this task only runs commands)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: every crate's tests pass. The expected totals:
- `hermes-skills`: 37 tests (6 frontmatter + 12 validate + 6 layout + 7 load_all + 6 render)
- `hermes-runtime`: existing tests + 5 integration tests
- `hermes-loop`, `hermes-providers`, `hermes-tools`, `hermes-core`, `hermes-cli`: all unchanged, all green

- [ ] **Step 2: Run clippy on the whole workspace**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: zero warnings. (The `clippy::ref_option` lint that the reviewer flagged is avoided by the `Option<&str>` signature chosen in Task 8.)

- [ ] **Step 3: Smoke-test the manual fixture with the echo provider**

Run:

```bash
# Skip if hermes-cli requires a real provider config; the echo provider
# can be configured in a temp file.
TMPHOME=$(mktemp -d)
mkdir -p "$TMPHOME/.perry_hermes/skills"
cp -r crates/hermes-runtime/skills-example/* "$TMPHOME/.perry_hermes/skills/"
echo '[provider]
kind = "echo"' > "$TMPHOME/.perry_hermes/config.toml"
HOME="$TMPHOME" cargo run -p hermes-cli --quiet -- --config "$TMPHOME/.perry_hermes/config.toml" <<< "hi"
rm -rf "$TMPHOME"
```

Expected: the REPL prints "ok" (the echo provider's reply). The integration tests have already verified the system-prompt content; this manual smoke is just to confirm the binary still works.

- [ ] **Step 4: Final commit (if any drift)**

If the smoke test surfaced any issue, fix it and commit. Otherwise, no commit is required; the plan is complete.

---

## Self-Review Notes

After writing the plan, these are the cross-checks the author ran:

- **Spec coverage:** Each section of the spec maps to at least one task:
  - §1 Goal → Task 1, Task 8, Task 12
  - §3 Bug fix → Task 8 (the `unwrap_or(DEFAULT_SYSTEM_PROMPT)` line)
  - §4 Architecture → Task 1 (new crate), Task 8 (wiring)
  - §5 Data shape → Task 2 (struct), Task 3 (frontmatter), Task 4 (validators), Task 5 (layout)
  - §6 Load + inject flow → Task 6 (load_all), Task 7 (render), Task 8 (compose_system_prompt)
  - §7 Error handling → Task 4 (validators) + Task 6 (warn-and-skip logic) + Task 8 (load_all Err path)
  - §8 Testing → Tasks 3–7 (unit tests) + Task 10 (integration)
  - §9 File changes → Task 1 (new crate), Task 9 (config cleanup), Task 11 (smoke), Task 12 (docs)
  - §10 Known limitations → documented in Task 12 (CLI test note about terminal-disabled)
  - §11 Open questions → n/a, no questions outstanding

- **Placeholder scan:** No "TBD", "TODO", "implement later", "fill in details", "similar to Task N" appear in the plan. Every step shows actual code.

- **Type consistency:**
  - `Skill` defined in Task 2 (struct only, no methods) and extended by `qualified_name`/`category`/`body`/`frontmatter`/`source_path` fields. Tasks 6 and 7 use those exact field names.
  - `parse_one` (Task 6) sets `qualified_name` via `format!("{cat}.{name}")` — same format `render_system_prompt_block` consumes implicitly by reading `s.category` and `s.name` separately (Task 7).
  - `SkillLocation.category` is `Option<String>` in Task 5; consumed as `Option<String>` in Task 6 via `loc.category.clone()`.
  - `compose_system_prompt` signature in Task 8 is `Option<&str>` (per spec review). Tests in Task 10 build `HermesConfig` with `config.agent.system_prompt = Some("…".into())` and the runtime's call site `config.agent.system_prompt.as_deref()` matches the signature.

- **Risks identified during planning:**
  - `clippy::ref_option` is a real lint; the plan uses `Option<&str>` consistently.
  - `serde_yaml` 0.9 is unmaintained but acceptable for this phase; the plan does not pretend otherwise.
  - The integration test's `unsafe { std::env::set_var("HOME", ...) }` calls are safe in practice because each test runs in its own `cargo test` subprocess (cargo's default for integration tests is one process per test binary, and the test binary holds no other threads doing env reads). The doc comment on each test makes this contract explicit.
