//! Integration tests for `BashTool`.
//!
//! Phase 3 minimum: the tool runs a real shell command, captures
//! combined stdout+stderr, and returns it as `ToolOutput.content`. We
//! test against the real `tokio::process::Command` path — bash on the
//! host is a fine test harness.

use std::path::PathBuf;

use hermes_core::tool::{Tool, ToolContext, ToolPermissions};
use hermes_tools::BashTool;
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

    // The content should contain what bash printed. We don't match
    // exact equality because the implementation may also append stderr
    // or an exit-code footer; what matters is that the user-visible
    // stdout reaches the LLM intact.
    assert!(
        out.content.contains("hello-from-bash"),
        "expected stdout in content, got: {:?}",
        out.content
    );
}

#[tokio::test]
async fn bash_tool_returns_nonzero_exit_in_content() {
    // `false` exits with code 1 but no output. The tool should still
    // succeed (the command ran); it surfaces the exit code in the
    // content so the LLM can see what went wrong.
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let out = tool
        .execute(json!({ "command": "false" }), ctx(), cancel)
        .await
        .expect("bash should run");

    assert!(
        out.content.contains("exit code 1"),
        "expected exit code in content, got: {:?}",
        out.content
    );
}

#[tokio::test]
async fn bash_tool_rejects_missing_command_arg() {
    let tool = BashTool::new();
    let cancel = CancellationToken::new();
    let err = tool
        .execute(json!({}), ctx(), cancel)
        .await
        .expect_err("missing 'command' should be rejected");

    let msg = err.to_string();
    assert!(
        msg.contains("command"),
        "expected error to mention 'command', got: {msg}"
    );
}
