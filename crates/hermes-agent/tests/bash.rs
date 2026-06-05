use std::path::PathBuf;

use hermes_agent::tools::BashTool;
use hermes_core::tool::{Tool, ToolContext, ToolPermissions};
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
