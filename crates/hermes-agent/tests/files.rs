use std::path::PathBuf;

use perry_hermes_agent::tools::{PatchTool, ReadFileTool, SearchFilesTool};
use perry_hermes_core::tool::{Tool, ToolContext, ToolPermissions};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

async fn with_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());
    ENV_LOCK.lock().await
}

fn ctx(working_dir: PathBuf) -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        working_dir,
        permissions: ToolPermissions { subprocess: false },
    }
}

fn parse(out: &perry_hermes_core::tool::ToolOutput) -> serde_json::Value {
    serde_json::from_str(&out.content).expect("read_file should return JSON")
}

#[tokio::test]
async fn read_file_returns_line_numbered_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap() }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("read should succeed");

    let v = parse(&out);
    let content = v["content"].as_str().unwrap();
    assert!(content.contains("1|alpha"), "got: {content}");
    assert!(content.contains("2|beta"));
    assert!(content.contains("3|gamma"));
    assert_eq!(v["total_lines"].as_i64(), Some(3));
    assert!(!v["truncated"].as_bool().unwrap_or(false));
}

#[tokio::test]
async fn read_file_supports_offset_and_limit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("lines.txt");
    let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    std::fs::write(&path, &body).unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "offset": 4, "limit": 2 }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("read should succeed");
    let v = parse(&out);
    let content = v["content"].as_str().unwrap();
    assert!(content.contains("4|line4"));
    assert!(content.contains("5|line5"));
    assert!(!content.contains("6|line6"));
    assert_eq!(v["total_lines"].as_i64(), Some(10));
    assert_eq!(v["truncated"].as_bool(), Some(true));
}

#[tokio::test]
async fn read_file_rejects_blocked_device_path() {
    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": "/dev/zero" }),
            ctx(PathBuf::from("/")),
            CancellationToken::new(),
        )
        .await
        .expect("device path returns JSON error, not Err");
    let v = parse(&out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(err.contains("device"), "got error: {err}");
}

#[tokio::test]
async fn read_file_rejects_known_binary_extension() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pic.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap() }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("binary returns JSON error");
    let v = parse(&out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(err.to_lowercase().contains("binary"), "got error: {err}");
    assert!(
        !err.contains("vision_analyze"),
        "binary error should not recommend vision_analyze (D15) — got: {err}"
    );
}

#[tokio::test]
async fn read_file_errors_when_offset_past_eof() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("small.txt");
    std::fs::write(&path, "a\nb\nc\n").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "offset": 100, "limit": 10 }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("read should return JSON");
    let v = parse(&out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("past end of file"),
        "expected 'past end of file' error, got: {err}"
    );
}

#[tokio::test]
async fn read_file_reports_not_found_with_similar_files() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("alpha.py");
    std::fs::write(&path, "print('alpha')").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": dir.path().join("alpha.rs").to_str().unwrap() }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("not-found returns JSON error");
    let v = parse(&out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(err.contains("not found"), "got error: {err}");
    let similar = v["similar_files"].as_array().expect("similar_files array");
    assert!(!similar.is_empty(), "expected at least one suggestion");
    assert!(similar
        .iter()
        .any(|s| s.as_str().unwrap_or_default().ends_with("alpha.py")));
}

#[tokio::test]
async fn read_file_resolves_relative_path_against_working_dir() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("nested.txt"), "from working dir").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": "nested.txt" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("read should succeed");
    let v = parse(&out);
    let content = v["content"].as_str().unwrap();
    assert!(content.contains("from working dir"));
}

#[tokio::test]
async fn read_file_caps_content_at_100k_chars() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big.txt");
    // 250K chars of 'a', each line 1000 chars.
    let mut body = String::new();
    for _ in 0..250 {
        body.push_str(&"a".repeat(1000));
        body.push('\n');
    }
    std::fs::write(&path, &body).unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "limit": 2000 }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("read should succeed");
    let v = parse(&out);
    let content = v["content"].as_str().unwrap();
    // Cap is 100K chars including the truncation notice; allow some slack for
    // the line-number prefix.
    assert!(
        content.chars().count() <= 110_000,
        "content len {}",
        content.chars().count()
    );
    assert!(content.contains("TRUNCATED") || content.len() < body.len());
}

// ---------------------------------------------------------------------------
// WriteFileTool tests
// ---------------------------------------------------------------------------

use perry_hermes_agent::tools::WriteFileTool;

#[tokio::test]
async fn write_file_creates_new_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("new.txt");

    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "content": "hello\n" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("write should succeed");
    let v = parse(&out);
    assert_eq!(v["bytes_written"].as_i64(), Some(6));
    assert_eq!(
        v["files_modified"].as_array().map(|a| a.len()),
        Some(1),
        "write_file should report files_modified"
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, "hello\n");
}

#[tokio::test]
async fn write_file_overwrites_existing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("existing.txt");
    std::fs::write(&path, "old content").unwrap();

    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "content": "new" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("write should succeed");
    let v = parse(&out);
    assert_eq!(v["bytes_written"].as_i64(), Some(3));
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, "new");
}

#[tokio::test]
async fn write_file_creates_parent_directories() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a/b/c/deep.txt");
    assert!(!path.parent().unwrap().exists());

    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "content": "deep" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("write should succeed");
    let v = parse(&out);
    assert_eq!(v["dirs_created"].as_bool(), Some(true));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep");
}

#[tokio::test]
async fn write_file_includes_resolved_path() {
    let dir = TempDir::new().unwrap();
    let _path = dir.path().join("rel.txt");

    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": "rel.txt", "content": "x" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("write should succeed");
    let v = parse(&out);
    let rp = v["resolved_path"].as_str().unwrap();
    assert!(rp.ends_with("rel.txt"), "resolved_path: {rp}");
}

#[tokio::test]
async fn write_file_rejects_internal_status_text() {
    let dir = TempDir::new().unwrap();
    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({
                "path": dir.path().join("status.txt").to_str().unwrap(),
                "content": "File unchanged since last read. The content from the earlier read_file result in this conversation is still current — refer to that instead of re-reading."
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("write should return JSON error");
    let v = parse(&out);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("internal read_file status text"));
}

#[tokio::test]
async fn write_file_rejects_cross_profile_write_without_override() {
    let _guard = with_env_lock().await;
    let dir = TempDir::new().unwrap();
    let perry_hermes_home = dir.path().join("profiles/current");
    std::fs::create_dir_all(&perry_hermes_home).unwrap();
    unsafe { std::env::set_var("PERRY_HERMES_HOME", &perry_hermes_home) };

    let target = dir.path().join("profiles/other/skills/demo/SKILL.md");
    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": target.to_str().unwrap(), "content": "demo" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("cross-profile write should return JSON error");
    let v = parse(&out);
    assert!(v["error"].as_str().unwrap().contains("cross-profile"));
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
}

#[tokio::test]
async fn write_file_rejects_perry_hermes_config_path() {
    let _guard = with_env_lock().await;
    let dir = TempDir::new().unwrap();
    let perry_hermes_home = dir.path().join("active-profile");
    std::fs::create_dir_all(&perry_hermes_home).unwrap();
    unsafe { std::env::set_var("PERRY_HERMES_HOME", &perry_hermes_home) };

    let target = perry_hermes_home.join("config.toml");
    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": target.to_str().unwrap(), "content": "unsafe = true" }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("config-path write should return JSON error");
    let v = parse(&out);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("Perry Hermes config file"));
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
}

#[tokio::test]
async fn write_file_allows_cross_profile_write_with_override() {
    let _guard = with_env_lock().await;
    let dir = TempDir::new().unwrap();
    let perry_hermes_home = dir.path().join("profiles/current");
    std::fs::create_dir_all(&perry_hermes_home).unwrap();
    unsafe { std::env::set_var("PERRY_HERMES_HOME", &perry_hermes_home) };

    let target = dir.path().join("profiles/other/skills/demo/SKILL.md");
    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({
                "path": target.to_str().unwrap(),
                "content": "demo",
                "cross_profile": true
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("cross-profile override should succeed");
    let v = parse(&out);
    assert!(v["resolved_path"].is_string());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "demo");
    unsafe { std::env::remove_var("PERRY_HERMES_HOME") };
}

#[tokio::test]
async fn write_file_does_not_leave_temp_file_on_success() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("clean.txt");

    let tool = WriteFileTool::new();
    tool.execute(
        json!({ "path": path.to_str().unwrap(), "content": "ok" }),
        ctx(dir.path().to_path_buf()),
        CancellationToken::new(),
    )
    .await
    .expect("write should succeed");

    // No leftover .perry-hermes-tmp-* file in the directory.
    let leaks: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".perry-hermes-tmp-")
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "found temp leaks: {:?}",
        leaks.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// PatchTool tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_schema_matches_reference_name_and_required_mode() {
    let tool = PatchTool::new();
    assert_eq!(tool.name(), "patch");
    let schema = tool.parameters_schema();
    let props = schema["properties"].as_object().unwrap();
    for key in [
        "mode",
        "path",
        "old_string",
        "new_string",
        "replace_all",
        "patch",
        "cross_profile",
    ] {
        assert!(props.contains_key(key), "missing parameter {key}");
    }
    assert_eq!(
        schema["required"].as_array().unwrap(),
        &vec![serde_json::Value::String("mode".to_string())]
    );
}

#[tokio::test]
async fn patch_replace_mode_edits_unique_match() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("demo.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let tool = PatchTool::new();
    let out = tool
        .execute(
            json!({
                "mode": "replace",
                "path": path.to_str().unwrap(),
                "old_string": "beta\n",
                "new_string": "BETA\n"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("patch should succeed");
    let v = parse(&out);
    assert!(v["diff"].as_str().unwrap_or_default().contains("-beta"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha\nBETA\ngamma\n"
    );
    assert_eq!(v["files_modified"].as_array().map(|a| a.len()), Some(1));
}

#[tokio::test]
async fn patch_replace_mode_rejects_duplicate_without_replace_all() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("demo.txt");
    std::fs::write(&path, "same\nsame\n").unwrap();

    let tool = PatchTool::new();
    let out = tool
        .execute(
            json!({
                "mode": "replace",
                "path": path.to_str().unwrap(),
                "old_string": "same",
                "new_string": "other"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("patch should return JSON error");
    let v = parse(&out);
    assert!(v["error"].as_str().unwrap_or_default().contains("multiple"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "same\nsame\n");
}

#[tokio::test]
async fn patch_replace_mode_supports_replace_all() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("demo.txt");
    std::fs::write(&path, "same\nsame\n").unwrap();

    let tool = PatchTool::new();
    tool.execute(
        json!({
            "mode": "replace",
            "path": path.to_str().unwrap(),
            "old_string": "same",
            "new_string": "other",
            "replace_all": true
        }),
        ctx(dir.path().to_path_buf()),
        CancellationToken::new(),
    )
    .await
    .expect("patch should succeed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "other\nother\n");
}

#[tokio::test]
async fn patch_v4a_add_update_delete_and_move_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("update.txt"), "old\nkeep\n").unwrap();
    std::fs::write(dir.path().join("delete.txt"), "remove me\n").unwrap();
    std::fs::write(dir.path().join("move.txt"), "move me\n").unwrap();

    let patch = "\
*** Begin Patch
*** Add File: added.txt
+created
*** Update File: update.txt
@@
-old
+new
 keep
*** Delete File: delete.txt
*** Move File: move.txt
*** Move to: moved.txt
*** End Patch";

    let tool = PatchTool::new();
    let out = tool
        .execute(
            json!({"mode": "patch", "patch": patch}),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("patch should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true), "output: {v}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("added.txt")).unwrap(),
        "created\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("update.txt")).unwrap(),
        "new\nkeep\n"
    );
    assert!(!dir.path().join("delete.txt").exists());
    assert!(!dir.path().join("move.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("moved.txt")).unwrap(),
        "move me\n"
    );
}

// ---------------------------------------------------------------------------
// SearchFilesTool tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_files_schema_matches_reference_name() {
    let tool = SearchFilesTool::new();
    assert_eq!(tool.name(), "search_files");
    let schema = tool.parameters_schema();
    let props = schema["properties"].as_object().unwrap();
    for key in [
        "pattern",
        "target",
        "path",
        "file_glob",
        "limit",
        "offset",
        "output_mode",
        "context",
    ] {
        assert!(props.contains_key(key), "missing parameter {key}");
    }
}

#[tokio::test]
async fn search_files_content_returns_line_column_and_content() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\nneedle here\n").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({"pattern": "needle", "path": dir.path().to_str().unwrap()}),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    let first = &v["matches"].as_array().unwrap()[0];
    assert_eq!(first["line"].as_u64(), Some(2));
    assert_eq!(first["column"].as_u64(), Some(1));
    assert_eq!(first["content"].as_str(), Some("needle here"));
}

#[tokio::test]
async fn search_files_file_glob_restricts_content_search() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "needle",
                "path": dir.path().to_str().unwrap(),
                "file_glob": "*.rs"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]["path"].as_str().unwrap().ends_with("a.rs"));
}

#[tokio::test]
async fn search_files_content_supports_offset_limit_and_files_only() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle 1\nneedle 2\nneedle 3\n").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "needle",
                "path": dir.path().to_str().unwrap(),
                "offset": 1,
                "limit": 1,
                "output_mode": "files_only"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    assert_eq!(v["files"].as_array().unwrap().len(), 1);
    assert_eq!(v["total"].as_u64(), Some(3));
    assert_eq!(v["truncated"].as_bool(), Some(true));
}

#[tokio::test]
async fn search_files_content_supports_count_mode() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle\nneedle\n").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "needle",
                "path": dir.path().to_str().unwrap(),
                "output_mode": "count"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    assert_eq!(
        v["counts"].as_array().unwrap()[0]["count"].as_u64(),
        Some(2)
    );
}

#[tokio::test]
async fn search_files_target_files_finds_glob_matches() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "*.rs",
                "target": "files",
                "path": dir.path().to_str().unwrap()
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].as_str().unwrap().ends_with("a.rs"));
}

#[tokio::test]
async fn search_files_target_files_applies_file_glob() {
    // D7: file_glob is honored in target='files' mode too, not just
    // target='content'. Without the fix, all files in the directory are
    // returned and the toml/rs split is lost.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.toml"), "").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();
    std::fs::write(dir.path().join("c.toml"), "").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "*",
                "target": "files",
                "path": dir.path().to_str().unwrap(),
                "file_glob": "*.toml"
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    let files = v["files"].as_array().unwrap();
    assert_eq!(
        files.len(),
        2,
        "file_glob='*.toml' should filter to the two .toml files"
    );
    for f in files {
        assert!(f.as_str().unwrap().ends_with(".toml"));
    }
}

#[tokio::test]
async fn search_files_content_supports_regex_pattern() {
    // Exercises the ripgrep backend specifically: a regex alternation
    // pattern that the pure-Rust walk fallback would treat as a literal
    // string and miss. If this test fails with `expected 2 matches`,
    // ripgrep is not installed — install it (`brew install ripgrep`,
    // `apt install ripgrep`) so the rg backend can take over.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

    let tool = SearchFilesTool::new();
    let out = tool
        .execute(
            json!({
                "pattern": "alpha|gamma",
                "path": dir.path().to_str().unwrap()
            }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("search should succeed");
    let v = parse(&out);
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(
        matches.len(),
        2,
        "expected regex 'alpha|gamma' to match both 'alpha' and 'gamma' — install ripgrep to enable the rg backend"
    );
    let contents: Vec<&str> = matches
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert!(contents.contains(&"alpha"));
    assert!(contents.contains(&"gamma"));
    assert!(!contents.contains(&"beta"));
}

#[tokio::test]
async fn patch_v4a_accepts_end_of_patch_with_optional_index() {
    // D4/D5: the parser must accept the "*** End of Patch" variant
    // (with "of") and an optional "[N]" suffix. Both forms appear in
    // documentation and existing tools.
    let dir = TempDir::new().unwrap();
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End of Patch [0]";

    let tool = PatchTool::new();
    let out = tool
        .execute(
            json!({"mode": "patch", "patch": patch}),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("patch should succeed");
    let v = parse(&out);
    assert_eq!(v["success"].as_bool(), Some(true), "output: {v}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "hello\n"
    );
}
