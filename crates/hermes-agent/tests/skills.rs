use std::path::PathBuf;

use hermes_agent::tools::{SkillListTool, SkillViewTool};
use hermes_core::tool::{Tool, ToolContext, ToolPermissions};
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

fn parse(out: hermes_core::tool::ToolOutput) -> serde_json::Value {
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(v["error"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("plugin"));
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
    let v = parse(out);
    assert_eq!(v["success"].as_bool(), Some(false));
    assert!(v["error"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("ambiguous"));
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
    let v = parse(out);
    let lf = &v["linked_files"];
    assert!(lf["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "references/file.md"));
    assert!(lf["references"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "references/notes.txt"));
    assert!(lf["templates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "templates/file.md"));
    assert!(lf["assets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "assets/file.md"));
    assert!(lf["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "scripts/file.py"));
}
