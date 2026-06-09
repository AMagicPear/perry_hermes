use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::qqbot::QqBotConfig;
use crate::telegram::TelegramConfig;

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
    /// Telegram platform config; `None` disables the adapter.
    pub telegram: Option<TelegramConfig>,
    /// QQ Bot platform config; `None` disables the adapter.
    pub qqbot: Option<QqBotConfig>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            sessions_dir: perry_hermes_agent::default_sessions_dir(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            allowed_users: HashMap::new(),
            telegram: None,
            qqbot: None,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_restricts_users() {
        let mut config = GatewayConfig::default();
        let mut allowed = HashSet::new();
        allowed.insert("12345".into());
        config.allowed_users.insert("telegram".into(), allowed);

        assert!(config.is_user_allowed("telegram", "12345"));
        assert!(!config.is_user_allowed("telegram", "99999"));
        assert!(config.is_user_allowed("discord", "anyone"));
    }
}
