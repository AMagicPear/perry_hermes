use std::path::PathBuf;
use std::time::Duration;

use perry_hermes_core::tool::{Tool, ToolContext, ToolPermissions};
use perry_hermes_skill_tools::tools::BashTool;
use perry_hermes_skill_tools::tools::process::ProcessTool;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ctx() -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        permissions: ToolPermissions { subprocess: true },
    }
}

#[tokio::test]
async fn bash_tool_runs_a_simple_command_and_returns_stdout() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let out = tool
        .execute(json!({ "command": "echo hello-from-bash" }), ctx(), cancel)
        .await
        .expect("bash should run");

    assert!(out.content.contains("hello-from-bash"));
}

#[tokio::test]
async fn bash_tool_returns_nonzero_exit_in_content() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let out = tool
        .execute(json!({ "command": "false" }), ctx(), cancel)
        .await
        .expect("bash should run");

    assert!(out.content.contains("exit code 1"));
}

#[tokio::test]
async fn bash_tool_rejects_missing_command_arg() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let err = tool
        .execute(json!({}), ctx(), cancel)
        .await
        .expect_err("missing 'command' should be rejected");

    assert!(err.to_string().contains("command"));
}

#[tokio::test]
async fn bash_tool_background_spawns_and_returns_session_id() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let out = tool
        .execute(
            json!({ "command": "echo bg-hello", "background": true }),
            ctx(),
            cancel,
        )
        .await
        .expect("background mode should succeed");

    assert!(out.content.contains("session_id"));
    assert!(out.content.contains("proc_"));
    assert!(out.content.contains("started"));
}

#[tokio::test]
async fn bash_tool_background_with_notify_on_complete() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let out = tool
        .execute(
            json!({
                "command": "echo notify-test",
                "background": true,
                "notify_on_complete": true
            }),
            ctx(),
            cancel,
        )
        .await
        .expect("background with notify should succeed");

    assert!(out.content.contains("notify_on_complete"));
    assert!(out.content.contains("true"));
}

#[tokio::test]
async fn process_tool_poll_after_background_spawn() {
    let bash = BashTool::new();
    let process = ProcessTool::new();
    let cancel = CancellationToken::new();

    // Spawn a background process.
    let out = bash
        .execute(
            json!({ "command": "echo poll-test-output", "background": true }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    // Wait for it to finish.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Poll for status.
    let poll_out = process
        .execute(
            json!({ "action": "poll", "session_id": session_id }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    assert!(poll_out.content.contains("finished"));
    assert!(poll_out.content.contains("poll-test-output"));
}

#[tokio::test]
async fn process_tool_list_shows_background_process() {
    let bash = BashTool::new();
    let process = ProcessTool::new();
    let cancel = CancellationToken::new();

    let out = bash
        .execute(
            json!({ "command": "echo list-test", "background": true }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    let list_out = process
        .execute(json!({ "action": "list" }), ctx(), cancel.clone())
        .await
        .unwrap();
    assert!(list_out.content.contains(&session_id));
}

#[tokio::test]
async fn process_tool_wait_returns_output() {
    let bash = BashTool::new();
    let process = ProcessTool::new();
    let cancel = CancellationToken::new();

    let out = bash
        .execute(
            json!({ "command": "echo wait-test-output", "background": true }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    let wait_out = process
        .execute(
            json!({ "action": "wait", "session_id": session_id, "timeout": 5 }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    assert!(wait_out.content.contains("wait-test-output"));
    assert!(wait_out.content.contains("timed_out"));
    assert!(wait_out.content.contains("false"));
}

#[tokio::test]
async fn process_tool_kill_terminates_entire_process_group() {
    // The background shell is started in a new process group, so killing
    // the session must also kill any children it forked. We verify by
    // asking the shell to background a `sleep` and only have that child
    // create a marker file — if process-group kill works, the child dies
    // before it touches the marker.
    let bash = BashTool::new();
    let process = ProcessTool::new();
    let cancel = CancellationToken::new();

    let marker = std::env::temp_dir().join("perry_hermes_kill_pgroup_marker.txt");
    let _ = std::fs::remove_file(&marker);

    let cmd = format!("(sleep 5; touch {}) & wait", marker.display());
    let out = bash
        .execute(
            json!({ "command": cmd, "background": true }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    // Give the shell a moment to fork the sleep child.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let kill_out = process
        .execute(
            json!({ "action": "kill", "session_id": session_id }),
            ctx(),
            cancel.clone(),
        )
        .await
        .unwrap();
    assert!(kill_out.content.to_lowercase().contains("killed"));

    // Wait past the 5s sleep deadline; if the child survived, the marker
    // would exist by now.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !marker.exists(),
        "child process survived after kill — process group was not terminated"
    );
}
