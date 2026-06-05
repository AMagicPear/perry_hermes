//! In-memory tool registry.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::tool::Tool;

/// Registry used by the loop. Tools are registered at startup, then looked up
/// by name while dispatching model tool calls.
#[derive(Default)]
pub struct InMemoryRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
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
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use crate::tool::{ToolContext, ToolOutput};

    struct NoopTool;

    #[async_trait]
    impl Tool for NoopTool {
        fn name(&self) -> &str {
            "core_noop"
        }

        fn description(&self) -> &str {
            "Core toolset: no-op placeholder for tests."
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({ "type": "object", "properties": {} })
        }

        fn toolset(&self) -> &'static str {
            "core"
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: ToolContext,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, crate::error::ToolError> {
            Ok(ToolOutput {
                content: String::new(),
            })
        }
    }

    #[test]
    fn register_lookup_and_schema() {
        let r = InMemoryRegistry::new().register(Arc::new(NoopTool));
        assert!(r.get("core_noop").is_some());
        assert!(r.get("nope").is_none());

        let s = r.schemas();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "core_noop");
        assert_eq!(
            s[0].description,
            "Core toolset: no-op placeholder for tests."
        );
    }
}
