use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Configuration for the gateway and all platform adapters.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Directory for session JSON files.
    pub sessions_dir: PathBuf,
    /// Working directory for the agent (tool context).
    pub working_dir: PathBuf,
    /// Allowed user IDs per platform. Empty map = allow all.
    /// Key: platform name (e.g. "telegram"), Value: set of allowed user IDs.
    pub allowed_users: HashMap<String, HashSet<String>>,
    /// Triggers that reset a session (e.g. "/new", "/reset").
    pub reset_triggers: Vec<String>,
    /// Optional system prompt for new sessions.
    pub system_prompt: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            sessions_dir: default_sessions_dir(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            allowed_users: HashMap::new(),
            reset_triggers: vec!["/new".into(), "/reset".into()],
            system_prompt: None,
        }
    }
}

impl GatewayConfig {
    /// Check whether `user_id` is authorized for `platform`.
    /// Returns true if the platform has no allow-list or the user is in it.
    pub fn is_user_allowed(&self, platform: &str, user_id: &str) -> bool {
        match self.allowed_users.get(platform) {
            Some(allowed) => allowed.is_empty() || allowed.contains(user_id),
            None => true,
        }
    }

    /// Check whether `text` is a reset trigger.
    pub fn is_reset_trigger(&self, text: &str) -> bool {
        self.reset_triggers
            .iter()
            .any(|trigger| text.trim() == trigger)
    }
}

/// Per-platform adapter configuration.
#[derive(Debug, Clone, Default)]
pub struct PlatformConfig {
    /// Whether this platform adapter is enabled.
    pub enabled: bool,
    /// Bot token (platform-specific).
    pub token: Option<String>,
}

fn default_sessions_dir() -> PathBuf {
    dirs_or_fallback().join("sessions")
}

fn dirs_or_fallback() -> PathBuf {
    if let Ok(home) = std::env::var("PERRY_HERMES_HOME") {
        return PathBuf::from(home);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".perry_hermes");
    }
    PathBuf::from(".perry_hermes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_allows_all_users() {
        let config = GatewayConfig::default();
        assert!(config.is_user_allowed("telegram", "anyone"));
    }

    #[test]
    fn empty_allow_list_permits_all() {
        let mut config = GatewayConfig::default();
        config
            .allowed_users
            .insert("telegram".into(), HashSet::new());
        assert!(config.is_user_allowed("telegram", "anyone"));
    }

    #[test]
    fn populated_allow_list_restricts() {
        let mut config = GatewayConfig::default();
        let mut allowed = HashSet::new();
        allowed.insert("12345".into());
        config.allowed_users.insert("telegram".into(), allowed);

        assert!(config.is_user_allowed("telegram", "12345"));
        assert!(!config.is_user_allowed("telegram", "99999"));
    }

    #[test]
    fn is_reset_trigger_matches_triggers() {
        let config = GatewayConfig::default();
        assert!(config.is_reset_trigger("/reset"));
        assert!(config.is_reset_trigger("/new"));
        assert!(config.is_reset_trigger("  /reset  "));
        assert!(!config.is_reset_trigger("hello"));
    }
}
