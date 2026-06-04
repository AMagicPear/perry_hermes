//! The `ToolRegistry` trait and an in-memory implementation.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::tool::Tool;

/// Resolves tool names to implementations and produces the JSON Schemas
/// passed to the LLM. Loops call `schemas()` once per iteration; tests and
/// the dispatcher call `get()`.
pub trait ToolRegistry: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
    fn names(&self) -> Vec<&str>;
    fn schemas(&self) -> Vec<ToolSchema>;
}

/// Default in-memory registry. Tools register themselves at startup via the
/// builder-style `register` method.
pub struct InMemoryRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool, consuming `self` so calls can be chained. Replaces
    /// any existing tool with the same name.
    pub fn register(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }
}

impl Default for InMemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry for InMemoryRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }
}

/// JSON-Schema-shaped description of a tool, sent to the LLM.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema (draft-07) describing the tool's arguments.
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use crate::tool::{ToolContext, ToolOutput};

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes back the message argument."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            })
        }
        async fn execute(
            &self,
            args: serde_json::Value,
            _ctx: ToolContext,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, crate::error::ToolError> {
            let msg = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolOutput {
                content: msg,
                attachments: vec![],
            })
        }
    }

    #[test]
    fn register_and_lookup() {
        let r = InMemoryRegistry::new().register(Arc::new(EchoTool));
        assert_eq!(r.names(), vec!["echo"]);
        assert!(r.get("echo").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn schemas_match_registered_tools() {
        let r = InMemoryRegistry::new().register(Arc::new(EchoTool));
        let s = r.schemas();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "echo");
        assert_eq!(s[0].description, "Echoes back the message argument.");
    }
}