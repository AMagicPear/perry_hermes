use std::sync::Arc;

use hermes_core::registry::InMemoryRegistry;
use hermes_tools::BashTool;

pub fn build_registry(disabled_toolsets: &[String]) -> InMemoryRegistry {
    if disabled_toolsets
        .iter()
        .any(|s| s == "core" || s == "terminal")
    {
        InMemoryRegistry::new()
    } else {
        InMemoryRegistry::new().register(Arc::new(BashTool::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_disables_terminal_toolset_from_registry() {
        let registry = build_registry(&["terminal".to_string()]);
        let names: Vec<_> = registry.schemas().into_iter().map(|schema| schema.name).collect();
        assert!(!names.iter().any(|name| name == "bash"));
    }
}
