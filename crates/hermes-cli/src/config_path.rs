use std::path::{Path, PathBuf};

use anyhow::bail;

pub(crate) fn resolve_config_path(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--config {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }

    let mut tried = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".perry_hermes").join("config.toml");
        tried.push(p.clone());
        if p.exists() {
            return Ok(p);
        }
    }
    let cwd_default = PathBuf::from("hermes.toml");
    tried.push(cwd_default.clone());
    if cwd_default.exists() {
        return Ok(cwd_default);
    }

    let mut msg = String::from("no hermes config found. Looked for:\n");
    for p in &tried {
        msg.push_str(&format!("  - {}\n", p.display()));
    }
    msg.push_str(
        "Pass --config <path> or create one of these. See examples/config/hermes.toml for a starter.",
    );
    bail!(msg);
}
