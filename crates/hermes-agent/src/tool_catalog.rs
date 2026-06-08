use std::path::Path;
use std::sync::Arc;

use perry_hermes_core::registry::InMemoryRegistry;

use perry_hermes_skill_tools::tools::{
    BashTool, PatchTool, ReadFileTool, SearchFilesTool, SkillListTool, SkillViewTool,
    WriteFileTool,
};

/// Wire all built-in tools into a fresh registry.
///
/// `skills_dir` is the resolved local skills directory (see
/// `crate::prompting::resolve_skills_dir`). It is read-only at this phase —
/// the four file / skills tools only need the path to find skill files.
pub fn build_registry(disabled_toolsets: &[String], skills_dir: &Path) -> InMemoryRegistry {
    let mut reg = InMemoryRegistry::new();

    // Accept both `terminal` (new) and `core` (legacy) so existing TOML
    // configs keep working. The tool advertises itself as `terminal`.
    if !disabled_toolsets
        .iter()
        .any(|s| s == "terminal" || s == "core")
    {
        reg = reg.register(Arc::new(BashTool::new()));
    }
    if !disabled_toolsets.iter().any(|s| s == "file") {
        reg = reg.register(Arc::new(ReadFileTool::new()));
        reg = reg.register(Arc::new(WriteFileTool::new()));
        reg = reg.register(Arc::new(PatchTool::new()));
        reg = reg.register(Arc::new(SearchFilesTool::new()));
    }
    if !disabled_toolsets.iter().any(|s| s == "skills") {
        reg = reg.register(Arc::new(SkillListTool::new(skills_dir.to_path_buf())));
        reg = reg.register(Arc::new(SkillViewTool::new(skills_dir.to_path_buf())));
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_skills_dir() -> PathBuf {
        PathBuf::from("/tmp/perry-hermes-test-skills")
    }

    #[test]
    fn runtime_disables_terminal_toolset_from_registry() {
        let registry = build_registry(&["terminal".to_string()], &test_skills_dir());
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(!names.iter().any(|n| n == "terminal"));
    }

    #[test]
    fn legacy_core_disables_shell_tool() {
        let registry = build_registry(&["core".to_string()], &test_skills_dir());
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(!names.iter().any(|n| n == "terminal"));
    }

    #[test]
    fn file_toolset_disables_read_write_patch_and_search() {
        let registry = build_registry(&["file".to_string()], &test_skills_dir());
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(!names.iter().any(|n| n == "read_file"));
        assert!(!names.iter().any(|n| n == "write_file"));
        assert!(!names.iter().any(|n| n == "patch"));
        assert!(!names.iter().any(|n| n == "search_files"));
    }

    #[test]
    fn skills_toolset_disables_list_and_view() {
        let registry = build_registry(&["skills".to_string()], &test_skills_dir());
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(!names.iter().any(|n| n == "skills_list"));
        assert!(!names.iter().any(|n| n == "skill_view"));
    }

    #[test]
    fn default_registry_includes_all_seven_tools() {
        let registry = build_registry(&[], &test_skills_dir());
        let names: Vec<_> = registry.schemas().into_iter().map(|s| s.name).collect();
        assert!(names.iter().any(|n| n == "terminal"));
        assert!(names.iter().any(|n| n == "read_file"));
        assert!(names.iter().any(|n| n == "write_file"));
        assert!(names.iter().any(|n| n == "patch"));
        assert!(names.iter().any(|n| n == "search_files"));
        assert!(names.iter().any(|n| n == "skills_list"));
        assert!(names.iter().any(|n| n == "skill_view"));
    }

    #[test]
    fn patch_schema_carries_reference_parameters() {
        let registry = build_registry(&[], &test_skills_dir());
        let patch = registry
            .get("patch")
            .expect("patch must be registered")
            .parameters_schema();
        let props = patch["properties"].as_object().expect("properties object");
        for key in [
            "mode",
            "path",
            "old_string",
            "new_string",
            "replace_all",
            "patch",
            "cross_profile",
        ] {
            assert!(props.contains_key(key), "patch missing parameter {key}");
        }
    }

    #[test]
    fn search_files_schema_carries_reference_parameters() {
        let registry = build_registry(&[], &test_skills_dir());
        let search = registry
            .get("search_files")
            .expect("search_files must be registered")
            .parameters_schema();
        let props = search["properties"].as_object().expect("properties object");
        for key in [
            "pattern",
            "target",
            "path",
            "file_glob",
            "limit",
            "offset",
            "output_mode",
            "context",
        ] {
            assert!(
                props.contains_key(key),
                "search_files missing parameter {key}"
            );
        }
    }
}
