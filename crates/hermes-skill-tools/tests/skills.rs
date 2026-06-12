use std::path::PathBuf;

use perry_hermes_core::tool::{Tool, ToolContext, ToolPermissions};
use perry_hermes_skill_tools::tools::{SkillCreateTool, SkillListTool, SkillViewTool};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn ctx() -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        permissions: ToolPermissions { subprocess: false },
    }
}

fn parse(out: &perry_hermes_core::tool::ToolOutput) -> serde_json::Value {
    serde_json::from_str(&out.content).expect("tool should return JSON")
}

fn write_skill(dir: &std::path::Path, name: &str, description: &str, body: &str) {
    let path = dir.join(name).join("SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    std::fs::write(&path, content).unwrap();
}

// ---------------------------------------------------------------------------
// SkillListTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn skills_list_returns_empty_for_missing_dir() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");

    let tool = SkillListTool::new(skills_dir.clone());
    let out = tool
        .execute(json!({}), ctx(), CancellationToken::new())
        .await
        .expect("list should not error on missing dir");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true));
    assert_eq!(v["count"].as_i64(), Some(0));
    // The spec says create-on-first-access; the dir should now exist.
    assert!(
        skills_dir.is_dir(),
        "skills dir should be created on first list"
    );
}

#[tokio::test]
async fn skills_list_returns_metadata_for_installed_skills() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "The alpha skill", "alpha body");
    write_skill(dir.path(), "beta", "The beta skill", "beta body");

    let tool = SkillListTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({}), ctx(), CancellationToken::new())
        .await
        .expect("list should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true));
    assert_eq!(v["count"].as_i64(), Some(2));
    let names: Vec<&str> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    let alpha = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "alpha")
        .unwrap();
    assert_eq!(alpha["description"].as_str().unwrap(), "The alpha skill");
    assert!(alpha.get("qualified_name").is_none());
    assert!(v["hint"].as_str().unwrap().contains("skill_view"));
}

#[tokio::test]
async fn skills_list_filters_by_category() {
    let dir = TempDir::new().unwrap();
    // category "mlops" via parent dir name
    std::fs::create_dir_all(dir.path().join("mlops").join("axolotl")).unwrap();
    std::fs::write(
        dir.path().join("mlops/axolotl/SKILL.md"),
        "---\nname: axolotl\ndescription: LLM fine-tuning\ncategory: mlops\n---\nbody",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("general").join("notes")).unwrap();
    std::fs::write(
        dir.path().join("general/notes/SKILL.md"),
        "---\nname: notes\ndescription: General notes\ncategory: general\n---\nbody",
    )
    .unwrap();

    let tool = SkillListTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(
            json!({"category": "mlops"}),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("list should succeed");
    let v = parse(&out);
    assert_eq!(v["count"].as_i64(), Some(1));
    assert_eq!(v["skills"][0]["name"].as_str().unwrap(), "axolotl");
}

#[tokio::test]
async fn skills_list_sorts_by_category_then_name() {
    let dir = TempDir::new().unwrap();
    // We can set category in frontmatter to control sort order.
    for (cat, name) in [("b", "zeta"), ("a", "alpha"), ("a", "beta"), ("b", "alpha")] {
        let p = dir.path().join(format!("{cat}/{name}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: d\ncategory: {cat}\n---\n"),
        )
        .unwrap();
    }
    let tool = SkillListTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({}), ctx(), CancellationToken::new())
        .await
        .expect("list should succeed");
    let v = parse(&out);
    let names: Vec<String> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    // After sort: (a,alpha), (a,beta), (b,alpha), (b,zeta)
    assert_eq!(names, vec!["alpha", "beta", "alpha", "zeta"]);
}

// ---------------------------------------------------------------------------
// SkillViewTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn skill_view_loads_main_body_with_frontmatter_stripped() {
    let dir = TempDir::new().unwrap();
    write_skill(
        dir.path(),
        "alpha",
        "The alpha skill",
        "# Alpha\n\nReal body",
    );

    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({"name": "alpha"}), ctx(), CancellationToken::new())
        .await
        .expect("view should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true));
    let content = v["content"].as_str().unwrap();
    assert!(
        !content.contains("name: alpha"),
        "frontmatter leaked: {content}"
    );
    assert!(content.contains("# Alpha"));
    assert!(content.contains("Real body"));
    assert_eq!(v["readiness_status"].as_str().unwrap(), "available");
    assert!(v["linked_files"].is_object());
}

#[tokio::test]
async fn skill_view_loads_linked_reference_file() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "The alpha skill", "# Alpha");
    std::fs::create_dir_all(dir.path().join("alpha/references")).unwrap();
    std::fs::write(
        dir.path().join("alpha/references/api.md"),
        "# API\n\nendpoint docs",
    )
    .unwrap();

    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(
            json!({"name": "alpha", "file_path": "references/api.md"}),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("view linked file should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true));
    assert!(v["content"].as_str().unwrap().contains("endpoint docs"));
    assert_eq!(v["file"].as_str().unwrap(), "references/api.md");
    assert_eq!(v["file_type"].as_str().unwrap(), ".md");
}

#[tokio::test]
async fn skill_view_reports_not_found_with_available_skills() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "desc", "body");
    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({"name": "missing"}), ctx(), CancellationToken::new())
        .await
        .expect("not-found returns JSON error");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(v["error"].as_str().unwrap().contains("missing"));
    let available = v["available_skills"].as_array().unwrap();
    assert!(available.iter().any(|n| n.as_str() == Some("alpha")));
}

#[tokio::test]
async fn skill_view_rejects_traversal_in_file_path() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "desc", "body");
    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(
            json!({"name": "alpha", "file_path": "../outside.txt"}),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("traversal returns JSON error");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(v["error"].as_str().unwrap().contains("escapes"));
}

#[tokio::test]
async fn skill_view_rejects_missing_linked_file() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "desc", "body");
    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(
            json!({"name": "alpha", "file_path": "references/missing.md"}),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("missing linked file returns JSON error");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(v["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn skill_view_rejects_plugin_qualified_name() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "desc", "body");
    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(
            json!({"name": "plugin:alpha"}),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("plugin qualifier returns JSON error");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("plugin")
    );
}

#[tokio::test]
async fn skill_view_returns_ambiguous_error_for_colliding_names() {
    let dir = TempDir::new().unwrap();
    // Two skills with the same frontmatter.name in different categories —
    // load_all() should yield both, and bare-name lookup must refuse to guess.
    std::fs::create_dir_all(dir.path().join("cat1/dupe")).unwrap();
    std::fs::write(
        dir.path().join("cat1/dupe/SKILL.md"),
        "---\nname: dupe\ndescription: first\ncategory: cat1\n---\nA",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("cat2/dupe")).unwrap();
    std::fs::write(
        dir.path().join("cat2/dupe/SKILL.md"),
        "---\nname: dupe\ndescription: second\ncategory: cat2\n---\nB",
    )
    .unwrap();
    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({"name": "dupe"}), ctx(), CancellationToken::new())
        .await
        .expect("collision returns JSON error");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("ambiguous")
    );
}

#[tokio::test]
async fn skill_view_linked_files_lists_references_templates_assets_scripts() {
    let dir = TempDir::new().unwrap();
    write_skill(dir.path(), "alpha", "desc", "body");
    let refs = dir.path().join("alpha/references");
    std::fs::create_dir_all(&refs).unwrap();
    std::fs::write(refs.join("file.md"), "x").unwrap();
    std::fs::write(refs.join("notes.txt"), "y").unwrap();

    let templates = dir.path().join("alpha/templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join("file.md"), "x").unwrap();

    let assets = dir.path().join("alpha/assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("file.md"), "x").unwrap();

    let scripts = dir.path().join("alpha/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("file.py"), "x").unwrap();

    let tool = SkillViewTool::new(dir.path().to_path_buf());
    let out = tool
        .execute(json!({"name": "alpha"}), ctx(), CancellationToken::new())
        .await
        .expect("view should succeed");
    let v = parse(&out);
    let lf = &v["linked_files"];
    assert!(
        lf["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "references/file.md")
    );
    assert!(
        lf["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "references/notes.txt")
    );
    assert!(
        lf["templates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "templates/file.md")
    );
    assert!(
        lf["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "assets/file.md")
    );
    assert!(
        lf["scripts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "scripts/file.py")
    );
}

// ---------------------------------------------------------------------------
// SkillCreateTool
// ---------------------------------------------------------------------------

fn read_skill_md(dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(name).join("SKILL.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?} failed: {e}", path))
}

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
async fn skill_create_writes_a_valid_skill_md_to_disk() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    let tool = SkillCreateTool::new(skills_dir.clone());

    let body =
        "# Rust error formatting\n\n## Overview\nUse thiserror for libraries, anyhow for apps.\n";
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
    assert!(
        v["path"]
            .as_str()
            .unwrap()
            .ends_with("rust-error-formatting/SKILL.md")
    );
    assert_eq!(v["size_bytes"].as_u64(), Some(content.len() as u64));
    assert!(v["note"].as_str().unwrap().contains("next session"));

    let on_disk = read_skill_md(&skills_dir, "rust-error-formatting");
    assert_eq!(on_disk, content, "on-disk file should equal input content");
}

#[tokio::test]
async fn skill_create_rejects_argument_and_name_violations() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let ctx_ = ctx();
    let cancel = CancellationToken::new();

    // Each row: (name, content, expected_error_substring, expected_field_or_None).
    // Use owned String for `name` and `content` so we can build cases with
    // computed values without leaking or temporary-lifetime errors.
    let big_content = format!(
        "---\nname: foo\ndescription: x\n---\n{}",
        "x".repeat(100_001)
    );
    let cases: Vec<(String, String, &'static str, Option<&'static str>)> = vec![
        // Missing args
        (
            String::new(),
            String::from("---\nname: PLACEHOLDER\ndescription: x\n---\nbody\n"),
            "missing 'name'",
            None,
        ),
        (
            String::from("foo"),
            String::new(),
            "missing 'content'",
            None,
        ),
        // Name shape
        (
            String::from("Foo"),
            String::from("---\nname: PLACEHOLDER\ndescription: x\n---\nbody\n"),
            "name",
            Some("name"),
        ),
        (
            String::from(".."),
            String::from("---\nname: PLACEHOLDER\ndescription: x\n---\nbody\n"),
            "'..'",
            Some("name"),
        ),
        (
            String::from("a<b"),
            String::from("---\nname: PLACEHOLDER\ndescription: x\n---\nbody\n"),
            "name",
            Some("name"),
        ),
        (
            "a".repeat(65),
            String::from("---\nname: PLACEHOLDER\ndescription: x\n---\nbody\n"),
            "name",
            Some("name"),
        ),
        // Content size
        (String::from("foo"), big_content, "100000", Some("content")),
    ];

    for (name, content, err_substr, field) in cases {
        let out = tool
            .execute(
                json!({ "name": name, "content": content }),
                ctx_.clone(),
                cancel.clone(),
            )
            .await
            .expect("tool should not error");
        let v = err(&out);
        let err_str = v["error"].as_str().unwrap_or("");
        assert!(
            err_str.contains(err_substr),
            "case {name:?} expected error containing {err_substr:?}, got {err_str:?}"
        );
        if let Some(f) = field {
            assert_eq!(
                v["field"].as_str(),
                Some(f),
                "case {name:?} expected field={f:?}, got {v}"
            );
        }
    }
}

#[tokio::test]
async fn skill_create_rejects_frontmatter_violations() {
    let dir = TempDir::new().unwrap();
    let tool = SkillCreateTool::new(dir.path().join("skills"));
    let ctx_ = ctx();
    let cancel = CancellationToken::new();

    // (name, content, expected_error_substring, expected_field_or_None)
    // Use owned Strings to keep test inputs alive for the iteration.
    let oversize_desc = format!(
        "---\nname: foo\ndescription: {}\n---\nbody\n",
        "x".repeat(1025)
    );
    let cases: Vec<(&str, String, &'static str, Option<&'static str>)> = vec![
        (
            "foo",
            String::from("no fence here"),
            "frontmatter",
            Some("content"),
        ),
        (
            "foo",
            String::from("---\nname: [unclosed\ndescription: x\n---\nbody\n"),
            "YAML",
            Some("content"),
        ),
        (
            "foo",
            String::from("---\njust-a-string\n---\nbody\n"),
            "mapping",
            Some("content"),
        ),
        (
            "foo",
            String::from("---\nname: bar\ndescription: x\n---\nbody\n"),
            "does not match",
            Some("name"),
        ),
        (
            "foo",
            String::from("---\nname: foo\n---\nbody\n"),
            "description",
            Some("description"),
        ),
        ("foo", oversize_desc, "description", Some("description")),
        (
            "foo",
            String::from("---\nname: foo\ndescription: x\n---\n   \n  \n"),
            "body",
            None,
        ),
    ];

    for (name, content, err_substr, field) in cases {
        let out = tool
            .execute(
                json!({ "name": name, "content": content }),
                ctx_.clone(),
                cancel.clone(),
            )
            .await
            .expect("tool should not error");
        let v = err(&out);
        let err_str = v["error"].as_str().unwrap_or("");
        assert!(
            err_str.contains(err_substr),
            "case name={name:?} expected error containing {err_substr:?}, got {err_str:?}"
        );
        if let Some(f) = field {
            assert_eq!(
                v["field"].as_str(),
                Some(f),
                "case name={name:?} expected field={f:?}, got {v}"
            );
        }
    }
}

#[tokio::test]
async fn skill_create_rejects_collision_and_leaves_no_tempfile() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    let tool = SkillCreateTool::new(skills_dir.clone());
    let ctx_ = ctx();
    let cancel = CancellationToken::new();

    // Pre-seed an existing skill with the same name.
    write_skill(&skills_dir, "foo", "old", "old body");

    let content = "---\nname: foo\ndescription: new desc\n---\nnew body\n";
    let out = tool
        .execute(
            json!({ "name": "foo", "content": content }),
            ctx_.clone(),
            cancel.clone(),
        )
        .await
        .expect("tool should not error");
    let v = err(&out);
    assert!(v["error"].as_str().unwrap().contains("already exists"));
    assert!(v["error"].as_str().unwrap().contains("write_file or patch"));
    assert_eq!(
        v["existing_path"].as_str().unwrap(),
        skills_dir.join("foo").join("SKILL.md").to_string_lossy()
    );

    // Pre-existing SKILL.md must be byte-for-byte unchanged.
    let on_disk = read_skill_md(&skills_dir, "foo");
    assert!(
        on_disk.contains("description: old"),
        "pre-existing skill must be preserved, got: {on_disk}"
    );
    assert!(
        !on_disk.contains("new body"),
        "collision must not overwrite, got: {on_disk}"
    );

    // Now a fresh create against a non-existing skill — atomic write should leave
    // only SKILL.md in the skill directory (no temp files).
    let content2 = "---\nname: bar\ndescription: x\n---\nbody\n";
    let out2 = tool
        .execute(
            json!({ "name": "bar", "content": content2 }),
            ctx_.clone(),
            cancel.clone(),
        )
        .await
        .expect("create should succeed");
    assert_eq!(parse(&out2)["success"].as_bool(), Some(true));

    let skill_dir = skills_dir.join("bar");
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
        .execute(
            json!({ "name": "foo", "content": content }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .expect("create should succeed");
    assert_eq!(parse(&out)["success"].as_bool(), Some(true));
    assert!(skills_dir.is_dir(), "skills dir should be created");
    assert!(
        skills_dir.join("foo").join("SKILL.md").is_file(),
        "SKILL.md should exist"
    );
}
