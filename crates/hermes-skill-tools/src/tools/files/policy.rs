use std::path::{Path, PathBuf};

use super::READ_DEDUP_STATUS_MESSAGE;

pub(super) fn sensitive_write_path_message(original: &str, resolved: &Path) -> Option<String> {
    let normalized = normalize_path_string(original);
    let resolved_str = resolved.to_string_lossy();
    let exact_blocked = ["/etc/passwd", "/etc/shadow"];
    let prefix_blocked = ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/var/run"];

    if exact_blocked
        .iter()
        .any(|entry| normalized == *entry || resolved_str == *entry)
        || prefix_blocked.iter().any(|prefix| {
            has_path_prefix(&normalized, prefix) || has_path_prefix(&resolved_str, prefix)
        })
    {
        return Some(format!(
            "Refusing to write to sensitive system path: {original}\nUse the terminal tool with sudo if you need to modify system files."
        ));
    }

    let perry_hermes_config = perry_hermes_config_path();
    if normalized == perry_hermes_config || resolved_str == perry_hermes_config {
        return Some(format!(
            "Refusing to write to Perry Hermes config file: {original}\nAgent cannot modify security-sensitive configuration. Edit ~/.perry_hermes/config.toml directly."
        ));
    }
    None
}

pub(super) fn cross_profile_write_message(resolved: &Path) -> Option<String> {
    let path = resolved.to_string_lossy();
    let marker = path.find("/profiles/")?;
    let rest = &path[marker + "/profiles/".len()..];
    let mut parts = rest.split('/');
    let profile = parts.next()?;
    let remainder: Vec<&str> = parts.collect();
    if remainder.len() < 2 {
        return None;
    }
    let scoped_dir = remainder[0];
    if !matches!(scoped_dir, "skills" | "plugins" | "cron" | "memories") {
        return None;
    }

    let active_profile = std::env::var("PERRY_HERMES_PROFILE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(current_profile_from_perry_hermes_home);
    match active_profile {
        Some(active) if active == profile => None,
        _ => Some(format!(
            "Refusing cross-profile write to Perry Hermes {scoped_dir} for profile '{profile}'. Pass cross_profile=true only after explicit user direction."
        )),
    }
}

pub(super) fn is_internal_file_status_text(content: &str) -> bool {
    let stripped = content.trim();
    if stripped.is_empty() {
        return false;
    }
    if stripped == READ_DEDUP_STATUS_MESSAGE {
        return true;
    }
    stripped.contains(READ_DEDUP_STATUS_MESSAGE)
        && stripped.len() <= 2 * READ_DEDUP_STATUS_MESSAGE.len()
}

pub(super) fn blocked_path_message(path: &Path) -> Option<String> {
    let literal = path.to_string_lossy();
    let literal_blocked = [
        "/dev/zero",
        "/dev/urandom",
        "/dev/random",
        "/dev/full",
        "/dev/stdin",
        "/dev/tty",
        "/dev/console",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/fd/0",
        "/dev/fd/1",
        "/dev/fd/2",
    ];
    for entry in literal_blocked {
        if literal == entry {
            return Some(format!(
                "Cannot read '{}': this is a device file that would block or produce infinite output.",
                literal
            ));
        }
    }
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return None,
    };
    for entry in literal_blocked {
        if canonical == entry {
            return Some(format!(
                "Cannot read '{}': this is a device file that would block or produce infinite output.",
                literal
            ));
        }
    }
    if canonical.starts_with("/proc/") {
        for tail in ["/fd/0", "/fd/1", "/fd/2", "/environ", "/cmdline", "/maps"] {
            if canonical.ends_with(tail) {
                return Some(format!(
                    "Cannot read '{}': this path can leak credentials or memory layout.",
                    literal
                ));
            }
        }
    }
    None
}

pub(super) fn is_binary_extension(ext: &str) -> bool {
    let lower = ext.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "heic"
            | "avif"
            | "ico"
            | "pdf"
            | "zip"
            | "tar"
            | "gz"
            | "tgz"
            | "bz2"
            | "xz"
            | "7z"
            | "rar"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "class"
            | "pyc"
            | "wasm"
            | "mp4"
            | "mp3"
            | "wav"
            | "flac"
            | "ogg"
            | "m4a"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "eot"
            | "psd"
            | "ai"
            | "sketch"
            | "fig"
            | "blend"
            | "glb"
            | "gltf"
            | "obj"
            | "fbx"
            | "stl"
            | "3ds"
            | "dae"
            | "db"
            | "sqlite"
            | "sqlite3"
            | "bin"
            | "dat"
            | "iso"
            | "dmg"
            | "deb"
            | "rpm"
            | "svg"
    )
}

pub(super) fn suggest_similar_files(path: &Path) -> Vec<String> {
    let dir = match path.parent() {
        Some(d) if d.is_dir() => d,
        _ => return Vec::new(),
    };
    let target_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n.to_ascii_lowercase(),
        None => return Vec::new(),
    };
    let stem = Path::new(&target_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = Path::new(&target_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut scored: Vec<(i32, String)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let lname = name.to_ascii_lowercase();
        let lstem = Path::new(&lname)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let lext = Path::new(&lname)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let score = if lname == target_name {
            100
        } else if !stem.is_empty() && lstem == stem {
            90
        } else if lname.starts_with(&target_name) || target_name.starts_with(&lname) {
            70
        } else if lname.contains(&target_name) {
            60
        } else if target_name.contains(&lname) && lname.len() > 2 {
            40
        } else if !ext.is_empty() && lext == ext {
            let common: std::collections::HashSet<char> = target_name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            let cand: std::collections::HashSet<char> = lname
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            let inter = common.intersection(&cand).count();
            let larger = common.len().max(cand.len());
            if larger > 0 && inter * 5 >= larger * 2 {
                30
            } else {
                0
            }
        } else {
            0
        };
        if score > 0 {
            scored.push((score, entry.path().to_string_lossy().into_owned()));
        }
    }
    scored.sort_by_key(|item| std::cmp::Reverse(item.0));
    scored.into_iter().take(5).map(|(_, p)| p).collect()
}

pub(super) fn temp_sibling(target: &Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| "write target has no parent directory".to_string())?;
    let pid = std::process::id();
    let mut tmp = parent.to_path_buf();
    let fname = match target.file_name().and_then(|s| s.to_str()) {
        Some(n) => format!(".perry-hermes-tmp-{n}.{pid}"),
        None => format!(".perry-hermes-tmp-{pid}"),
    };
    tmp.push(fname);
    Ok(tmp)
}

fn current_profile_from_perry_hermes_home() -> Option<String> {
    let path = perry_hermes_core::home::resolve_home_dir()?;
    let parent = path.parent()?;
    // If home dir is `<root>/profiles/<name>`, parent is `<root>/profiles`
    // and the profile name is the last component of `path`.
    if parent.file_name().and_then(|s| s.to_str()) == Some("profiles") {
        return path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
    }
    None
}

fn perry_hermes_config_path() -> String {
    perry_hermes_core::home::resolve_home_dir()
        .unwrap_or_else(|| PathBuf::from("~/.perry_hermes"))
        .join("config.toml")
        .to_string_lossy()
        .into_owned()
}

fn normalize_path_string(input: &str) -> String {
    if let Some(stripped) = input.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home)
            .join(stripped)
            .to_string_lossy()
            .into_owned();
    }
    input.to_string()
}

fn has_path_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
