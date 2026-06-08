use std::path::Path;

use serde_json::Value;

pub(super) fn discover_linked_files(skill_root: &Path) -> Value {
    let mut out = serde_json::Map::new();
    for bucket in ["references", "templates", "assets", "scripts"] {
        let dir = skill_root.join(bucket);
        let names = collect_bucket_files(skill_root, &dir, bucket_specific_extensions(bucket));
        if !names.is_empty() {
            out.insert(
                bucket.to_string(),
                Value::Array(names.into_iter().map(Value::String).collect()),
            );
        }
    }
    let other: Vec<String> = match std::fs::read_dir(skill_root) {
        Ok(e) => e
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && n != "SKILL.md")
            .filter(|n| {
                !["references", "templates", "assets", "scripts"]
                    .iter()
                    .any(|b| b == n)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if !other.is_empty() {
        out.insert(
            "other".to_string(),
            Value::Array(other.into_iter().map(Value::String).collect()),
        );
    }
    Value::Object(out)
}

fn bucket_specific_extensions(bucket: &str) -> Option<&'static [&'static str]> {
    match bucket {
        "references" => None,
        "templates" => Some(&["md", "py", "yaml", "yml", "json", "tex", "sh"]),
        "scripts" => Some(&["py", "sh", "bash", "js", "ts", "rb"]),
        "assets" => None,
        _ => None,
    }
}

fn collect_bucket_files(
    skill_root: &Path,
    dir: &Path,
    allowed_extensions: Option<&[&str]>,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name_hidden = entry.file_name().to_string_lossy().starts_with('.');
        if name_hidden {
            continue;
        }
        if path.is_dir() {
            files.extend(collect_bucket_files(skill_root, &path, allowed_extensions));
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Some(exts) = allowed_extensions {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if !exts.contains(&ext) {
                continue;
            }
        }
        if let Ok(rel) = path.strip_prefix(skill_root) {
            files.push(rel.to_string_lossy().into_owned());
        }
    }
    files.sort();
    files
}
