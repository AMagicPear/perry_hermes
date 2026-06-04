//! The `ToolRegistry` trait and an in-memory implementation.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use serde::Serialize;

use crate::tool::Tool;

/// Resolves tool names to implementations, produces the JSON Schemas passed
/// to the LLM, and answers toolset / availability questions. The loop calls
/// `schemas()` once per iteration; tests and the dispatcher call `get()`.
pub trait ToolRegistry: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
    fn names(&self) -> Vec<&str>;
    fn schemas(&self) -> Vec<ToolSchema>;

    /// Distinct toolset names of all registered tools, sorted
    /// alphabetically for deterministic output.
    fn toolsets(&self) -> Vec<&'static str>;

    /// All tools belonging to the given toolset, sorted by tool name for
    /// deterministic output.
    fn tools_in_toolset(&self, toolset: &str) -> Vec<Arc<dyn Tool>>;
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
        let mut names: Vec<&str> = self.tools.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
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

    fn toolsets(&self) -> Vec<&'static str> {
        let set: BTreeSet<&'static str> = self.tools.values().map(|t| t.toolset()).collect();
        set.into_iter().collect()
    }

    fn tools_in_toolset(&self, toolset: &str) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = self
            .tools
            .values()
            .filter(|t| t.toolset() == toolset)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name().cmp(b.name()));
        out
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

    /// A no-op placeholder tool. Belongs to the "core" toolset. Exists only
    /// to verify the registry's register / lookup / schema / toolset
    /// machinery; it does nothing useful at runtime.
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
                attachments: vec![],
            })
        }
    }

    /// A second tool in the "core" toolset — used to verify the grouping
    /// methods return more than one entry.
    struct CorePingTool;

    #[async_trait]
    impl Tool for CorePingTool {
        fn name(&self) -> &str {
            "core_ping"
        }
        fn description(&self) -> &str {
            "Core toolset: always-available ping."
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
                content: "pong".into(),
                attachments: vec![],
            })
        }
    }

    /// A tool in a different toolset, to verify cross-toolset filtering.
    struct ExperimentalReadTool;

    #[async_trait]
    impl Tool for ExperimentalReadTool {
        fn name(&self) -> &str {
            "experimental_read"
        }
        fn description(&self) -> &str {
            "Experimental toolset: placeholder read tool."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({ "type": "object", "properties": {} })
        }
        fn toolset(&self) -> &'static str {
            "experimental"
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: ToolContext,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, crate::error::ToolError> {
            Ok(ToolOutput {
                content: String::new(),
                attachments: vec![],
            })
        }
    }

    // ── baseline behaviour (the original phase 0 tests, adapted) ─────

    #[test]
    fn register_and_lookup() {
        let r = InMemoryRegistry::new().register(Arc::new(NoopTool));
        assert_eq!(r.names(), vec!["core_noop"]);
        assert!(r.get("core_noop").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn schemas_match_registered_tools() {
        let r = InMemoryRegistry::new().register(Arc::new(NoopTool));
        let s = r.schemas();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "core_noop");
        assert_eq!(s[0].description, "Core toolset: no-op placeholder for tests.");
    }

    // ── toolset grouping (P0 fix from plans/hermes-comparison.md) ─────

    #[test]
    fn toolsets_lists_distinct_sorted_names() {
        let r = InMemoryRegistry::new()
            .register(Arc::new(NoopTool))
            .register(Arc::new(CorePingTool))
            .register(Arc::new(ExperimentalReadTool));
        assert_eq!(r.toolsets(), vec!["core", "experimental"]);
    }

    #[test]
    fn tools_in_toolset_filters_correctly_and_sorts() {
        let r = InMemoryRegistry::new()
            .register(Arc::new(NoopTool))
            .register(Arc::new(CorePingTool))
            .register(Arc::new(ExperimentalReadTool));

        let core = r.tools_in_toolset("core");
        assert_eq!(core.len(), 2);
        assert_eq!(core[0].name(), "core_noop");
        assert_eq!(core[1].name(), "core_ping");

        let experimental = r.tools_in_toolset("experimental");
        assert_eq!(experimental.len(), 1);
        assert_eq!(experimental[0].name(), "experimental_read");

        let missing = r.tools_in_toolset("nope");
        assert!(missing.is_empty());
    }
}
