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
                toolset: t.toolset().to_string(),
                is_async: t.is_async(),
                requires_env: t.requires_env().iter().map(|s| s.to_string()).collect(),
                max_result_size_chars: t.max_result_size_chars(),
                emoji: t.emoji().map(|s| s.to_string()),
                available: t.check_available(),
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
    pub toolset: String,
    pub is_async: bool,
    pub requires_env: Vec<String>,
    pub max_result_size_chars: Option<usize>,
    pub emoji: Option<String>,
    pub available: bool,
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

        fn is_async(&self) -> bool {
            true
        }

        fn requires_env(&self) -> &[&str] {
            &["NOOP_ENV"]
        }

        fn max_result_size_chars(&self) -> Option<usize> {
            Some(1234)
        }

        fn emoji(&self) -> Option<&str> {
            Some("T")
        }

        fn check_available(&self) -> bool {
            false
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
        assert_eq!(s[0].toolset, "core");
        assert!(s[0].is_async);
        assert_eq!(s[0].requires_env, vec!["NOOP_ENV"]);
        assert_eq!(s[0].max_result_size_chars, Some(1234));
        assert_eq!(s[0].emoji.as_deref(), Some("T"));
        assert!(!s[0].available);
    }
}
