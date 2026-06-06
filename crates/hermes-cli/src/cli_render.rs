pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

pub(crate) fn tool_emoji(name: &str) -> &'static str {
    match name {
        "bash" | "terminal" => "⚡",
        "read_file" | "write_file" => "📄",
        "search_files" => "🔰",
        "memory" => "🧠",
        _ => "🔧",
    }
}
