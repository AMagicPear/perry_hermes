//! QQ Bot platform configuration.

use thiserror::Error;

/// Configuration for the QQ Bot adapter.
///
/// `app_id` and `app_secret` are checked first; if `None`, the env vars
/// named in `app_id_env` / `app_secret_env` are read.
///
/// `intents` is a raw `u32` bitmask. When 0, [`QqBotConfig::build_intents`]
/// falls back to `Intents::PUBLIC_MESSAGES` (covers C2C + group @).
#[derive(Debug, Clone)]
pub struct QqBotConfig {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub app_id_env: String,
    pub app_secret_env: String,
    pub sandbox: bool,
    pub intents: u32,
}

impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            app_id: None,
            app_secret: None,
            app_id_env: "QQ_BOT_APP_ID".into(),
            app_secret_env: "QQ_BOT_APP_SECRET".into(),
            sandbox: false,
            intents: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum QqBotConfigError {
    #[error("qqbot: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

impl QqBotConfig {
    /// Returns `(app_id, app_secret)`. Falls back to env vars when
    /// `self.app_id` / `self.app_secret` are `None`.
    pub fn resolve(&self) -> Result<(String, String), QqBotConfigError> {
        let app_id = match &self.app_id {
            Some(v) => v.clone(),
            None => read_env(&self.app_id_env)?,
        };
        let app_secret = match &self.app_secret {
            Some(v) => v.clone(),
            None => read_env(&self.app_secret_env)?,
        };
        Ok((app_id, app_secret))
    }

    /// Convert `self.intents` into the lib's typed bitflags.
    /// `intents == 0` falls back to `Intents::PUBLIC_MESSAGES`.
    pub fn build_intents(&self) -> qq_bot_rs::Intents {
        if self.intents == 0 {
            qq_bot_rs::Intents::PUBLIC_MESSAGES
        } else {
            // Safe: `intents` is a u32, same as Intents::bits.
            qq_bot_rs::Intents::from_bits_truncate(self.intents)
        }
    }
}

fn read_env(var: &str) -> Result<String, QqBotConfigError> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(QqBotConfigError::MissingCredential { var: var.into() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_values() {
        let cfg = QqBotConfig {
            app_id: Some("id123".into()),
            app_secret: Some("secret456".into()),
            ..QqBotConfig::default()
        };
        assert_eq!(cfg.resolve().unwrap(), ("id123".into(), "secret456".into()));
    }

    #[test]
    fn resolve_falls_back_to_env() {
        unsafe { std::env::set_var("QQ_BOT_TEST_ID", "id_from_env") };
        unsafe { std::env::set_var("QQ_BOT_TEST_SECRET", "secret_from_env") };
        let cfg = QqBotConfig {
            app_id: None,
            app_secret: None,
            app_id_env: "QQ_BOT_TEST_ID".into(),
            app_secret_env: "QQ_BOT_TEST_SECRET".into(),
            ..QqBotConfig::default()
        };
        let (id, secret) = cfg.resolve().unwrap();
        assert_eq!(id, "id_from_env");
        assert_eq!(secret, "secret_from_env");
        unsafe { std::env::remove_var("QQ_BOT_TEST_ID") };
        unsafe { std::env::remove_var("QQ_BOT_TEST_SECRET") };
    }

    #[test]
    fn resolve_errors_when_missing() {
        let cfg = QqBotConfig {
            app_id_env: "QQ_BOT_NONEXISTENT_VAR_42".into(),
            app_secret_env: "QQ_BOT_NONEXISTENT_VAR_42".into(),
            ..QqBotConfig::default()
        };
        assert!(matches!(
            cfg.resolve(),
            Err(QqBotConfigError::MissingCredential { .. })
        ));
    }

    #[test]
    fn build_intents_defaults_to_public_messages() {
        let cfg = QqBotConfig::default();
        let intents = cfg.build_intents();
        assert!(intents.contains(qq_bot_rs::Intents::PUBLIC_MESSAGES));
    }

    #[test]
    fn build_intents_preserves_custom_bits() {
        // GUILDS (1 << 0) | GUILD_MEMBERS (1 << 1) = 0b11 — both are
        // defined bits in the bitflags set, so `from_bits_truncate`
        // preserves them.
        let cfg = QqBotConfig {
            intents: 0b11,
            ..QqBotConfig::default()
        };
        let intents = cfg.build_intents();
        assert_eq!(intents.bits(), 0b11);
        assert!(intents.contains(qq_bot_rs::Intents::GUILDS));
        assert!(intents.contains(qq_bot_rs::Intents::GUILD_MEMBERS));
    }
}
