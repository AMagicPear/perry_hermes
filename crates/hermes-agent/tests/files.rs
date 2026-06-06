use std::path::PathBuf;

use hermes_agent::tools::ReadFileTool;
use hermes_core::tool::{Tool, ToolContext, ToolPermissions};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn ctx(working_dir: PathBuf) -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        working_dir,
        permissions: ToolPermissions { subprocess: false },
    }
}

fn parse(out: hermes_core::tool::ToolOutput) -> serde_json::Value {
    serde_json::from_str(&out.content).expect("read_file should return JSON")
}

#[tokio::test]
async fn read_file_returns_line_numbered_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(json!({ "path": path.to_str().unwrap() }), ctx(dir.path().to_path_buf()), CancellationToken::new())
        .await
        .expect("read should succeed");

    let v = parse(out);
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
    let v = parse(out);
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
        .execute(json!({ "path": "/dev/zero" }), ctx(PathBuf::from("/")), CancellationToken::new())
        .await
        .expect("device path returns JSON error, not Err");
    let v = parse(out);
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
        .execute(json!({ "path": path.to_str().unwrap() }), ctx(dir.path().to_path_buf()), CancellationToken::new())
        .await
        .expect("binary returns JSON error");
    let v = parse(out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(err.to_lowercase().contains("binary"), "got error: {err}");
}

#[tokio::test]
async fn read_file_reports_not_found_with_similar_files() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("alpha.py");
    std::fs::write(&path, "print('alpha')").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(json!({ "path": dir.path().join("alpha.rs").to_str().unwrap() }), ctx(dir.path().to_path_buf()), CancellationToken::new())
        .await
        .expect("not-found returns JSON error");
    let v = parse(out);
    let err = v["error"].as_str().unwrap_or_default();
    assert!(err.contains("not found"), "got error: {err}");
    let similar = v["similar_files"].as_array().expect("similar_files array");
    assert!(!similar.is_empty(), "expected at least one suggestion");
    assert!(similar.iter().any(|s| s.as_str().unwrap_or_default().ends_with("alpha.py")));
}

#[tokio::test]
async fn read_file_resolves_relative_path_against_working_dir() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("nested.txt"), "from working dir").unwrap();

    let tool = ReadFileTool::new();
    let out = tool
        .execute(json!({ "path": "nested.txt" }), ctx(dir.path().to_path_buf()), CancellationToken::new())
        .await
        .expect("read should succeed");
    let v = parse(out);
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
        .execute(json!({ "path": path.to_str().unwrap(), "limit": 2000 }), ctx(dir.path().to_path_buf()), CancellationToken::new())
        .await
        .expect("read should succeed");
    let v = parse(out);
    let content = v["content"].as_str().unwrap();
    // Cap is 100K chars including the truncation notice; allow some slack for
    // the line-number prefix.
    assert!(content.chars().count() <= 110_000, "content len {}", content.chars().count());
    assert!(content.contains("TRUNCATED") || content.len() < body.len());
}

// ---------------------------------------------------------------------------
// WriteFileTool tests
// ---------------------------------------------------------------------------

use hermes_agent::tools::WriteFileTool;

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
    let v = parse(out);
    assert_eq!(v["bytes_written"].as_i64(), Some(6));
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
    let v = parse(out);
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
    let v = parse(out);
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
    let v = parse(out);
    let rp = v["resolved_path"].as_str().unwrap();
    assert!(rp.ends_with("rel.txt"), "resolved_path: {rp}");
}

#[tokio::test]
async fn write_file_accepts_cross_profile_arg() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cp.txt");
    let tool = WriteFileTool::new();
    let out = tool
        .execute(
            json!({ "path": path.to_str().unwrap(), "content": "x", "cross_profile": true }),
            ctx(dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect("cross_profile=true should not error in this build");
    let v = parse(out);
    assert!(v["resolved_path"].is_string());
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

    // No leftover .hermes-tmp-* file in the directory.
    let leaks: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".hermes-tmp-")
        })
        .collect();
    assert!(leaks.is_empty(), "found temp leaks: {:?}", leaks.iter().map(|e| e.path()).collect::<Vec<_>>());
}
