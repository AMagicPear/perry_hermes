use std::path::PathBuf;

pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a careful assistant with access to a `bash` tool. \
Use it to inspect the system or run shell commands when needed. When you have enough information \
to answer, give a concise final response — do not call tools again.";

fn default_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".perry_hermes").join("skills"))
}

pub fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let skills = match default_skills_dir() {
        Some(d) => match hermes_skills::load_all(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("failed to scan skills dir {}: {e}", d.display());
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);
    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}
