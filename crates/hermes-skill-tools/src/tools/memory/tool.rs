//! `MemoryTool` — LLM-facing tool for adding/replacing/removing/reading
//! entries in `MEMORY.md` and `USER.md`.

use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::error::ToolError;
use perry_hermes_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::store::{MemoryError, MemoryStore, MemoryTarget};

/// Description shown to the model. Adapted from hermes-agent's
/// `MEMORY_SCHEMA` description; behavioral contract preserved.
const MEMORY_TOOL_DESCRIPTION: &str = "Save durable information to persistent memory that survives across sessions. \
Memory is injected into future turns, so keep it compact and focused on facts that will still matter later.\n\
\n\
WHEN TO SAVE (do this proactively, don't wait to be asked):\n\
- User corrects you or says 'remember this' / 'don't do that again'\n\
- User shares a preference, habit, or personal detail (name, role, timezone, coding style)\n\
- You discover something about the environment (OS, installed tools, project structure)\n\
- You learn a convention, API quirk, or workflow specific to this user's setup\n\
- You identify a stable fact that will be useful again in future sessions\n\
\n\
PRIORITY: User preferences and corrections > environment facts > procedural knowledge. \
The most valuable memory prevents the user from having to repeat themselves.\n\
\n\
Do NOT save task progress, session outcomes, completed-work logs, or temporary TODO state to memory.\n\
\n\
TWO TARGETS:\n\
- 'user': who the user is -- name, role, preferences, communication style\n\
- 'memory': your notes -- environment facts, project conventions, tool quirks, lessons learned\n\
\n\
ACTIONS: add (new entry), replace (update existing -- old_text identifies it), \
remove (delete -- old_text identifies it), read (list current entries).";

pub struct MemoryTool {
    store: Arc<MemoryStore>,
}

impl MemoryTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        MEMORY_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove", "read"],
                    "description": "The action to perform."
                },
                "target": {
                    "type": "string",
                    "enum": ["memory", "user"],
                    "description": "Which memory store: 'memory' for personal notes, 'user' for user profile."
                },
                "content": {
                    "type": "string",
                    "description": "The entry content. Required for add and replace."
                },
                "old_text": {
                    "type": "string",
                    "description": "Short unique substring identifying the entry to replace or remove."
                }
            },
            "required": ["action", "target"]
        })
    }

    fn toolset(&self) -> &'static str {
        "memory"
    }

    fn emoji(&self) -> Option<&str> {
        Some("🧠")
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'action'".into()))?;
        let target = parse_target(args.get("target"))?;

        // Dispatch by action. Each store method returns a uniform
        // (entries, entry_count) pair which we serialize to JSON.
        let json = match action {
            "add" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("content required for 'add'".into())
                    })?;
                match self.store.add(target, content.to_string()).await {
                    Ok(value) => success_json(target, &value.entries, value.entry_count),
                    Err(err) => error_json(&err),
                }
            }
            "replace" => {
                let old = args
                    .get("old_text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("old_text required for 'replace'".into())
                    })?;
                let new = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("content required for 'replace'".into())
                    })?;
                match self.store.replace(target, old, new.to_string()).await {
                    Ok(value) => success_json(target, &value.entries, value.entry_count),
                    Err(err) => error_json(&err),
                }
            }
            "remove" => {
                let old = args
                    .get("old_text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("old_text required for 'remove'".into())
                    })?;
                match self.store.remove(target, old).await {
                    Ok(value) => success_json(target, &value.entries, value.entry_count),
                    Err(err) => error_json(&err),
                }
            }
            "read" => match self.store.read(target).await {
                Ok(value) => success_json(target, &value.entries, value.entry_count),
                Err(err) => error_json(&err),
            },
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "unknown action '{other}'; use add, replace, remove, read"
                )));
            }
        };
        Ok(ToolOutput {
            content: serde_json::to_string(&json).map_err(|e| {
                ToolError::Execution(format!("failed to serialize memory result: {e}"))
            })?,
        })
    }
}

fn parse_target(value: Option<&Value>) -> Result<MemoryTarget, ToolError> {
    let s = value
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'target'".into()))?;
    match s {
        "memory" => Ok(MemoryTarget::Memory),
        "user" => Ok(MemoryTarget::User),
        other => Err(ToolError::InvalidArgs(format!(
            "invalid target '{other}'; use 'memory' or 'user'"
        ))),
    }
}

fn success_json(target: MemoryTarget, entries: &[String], entry_count: usize) -> Value {
    serde_json::json!({
        "success": true,
        "target": target,
        "entries": entries,
        "entry_count": entry_count,
    })
}

fn error_json(err: &MemoryError) -> Value {
    serde_json::json!({
        "success": false,
        "error": err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn create_store(path: std::path::PathBuf) -> Arc<MemoryStore> {
        let store = MemoryStore::load(super::super::store::MemoryConfig {
            memories_dir: path,
        })
        .await
        .unwrap();
        Arc::new(store)
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            working_dir: std::path::PathBuf::from("/tmp"),
            permissions: Default::default(),
        }
    }

    #[tokio::test]
    async fn add_action_returns_success_json() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "add", "target": "memory", "content": "hello" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["target"], "memory");
        assert_eq!(v["entry_count"], 1);
    }

    #[tokio::test]
    async fn add_with_empty_content_returns_error_json() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "add", "target": "memory", "content": "  " }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], false);
        assert!(v["error"].as_str().unwrap().contains("content"));
    }

    #[tokio::test]
    async fn missing_action_returns_invalid_args() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn missing_target_returns_invalid_args() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "read" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn invalid_target_returns_invalid_args() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "read", "target": "global" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn unknown_action_returns_invalid_args() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let err = tool
            .execute(
                json!({ "action": "purge", "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn read_action_returns_empty_entries() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store);
        let out = tool
            .execute(
                json!({ "action": "read", "target": "memory" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["entry_count"], 0);
        assert_eq!(v["entries"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn replace_action_dispatches_to_store() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store.clone());
        // Pre-populate.
        tool.execute(
            json!({ "action": "add", "target": "memory", "content": "old text" }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        // Replace.
        let out = tool
            .execute(
                json!({
                    "action": "replace",
                    "target": "memory",
                    "old_text": "old text",
                    "content": "new text"
                }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["entries"], json!(["new text"]));
        assert_eq!(v["entry_count"], 1);
    }

    #[tokio::test]
    async fn remove_action_dispatches_to_store() {
        let dir = temp_dir();
        let store = create_store(dir.path().to_path_buf()).await;
        let tool = MemoryTool::new(store.clone());
        tool.execute(
            json!({ "action": "add", "target": "memory", "content": "doomed" }),
            ctx(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let out = tool
            .execute(
                json!({ "action": "remove", "target": "memory", "old_text": "doomed" }),
                ctx(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["entry_count"], 0);
    }
}