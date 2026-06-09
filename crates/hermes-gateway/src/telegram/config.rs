//! Telegram platform configuration.

use thiserror::Error;

use super::adapter::TelegramAdapter;

/// Configuration for the Telegram adapter.
///
/// `token` is checked first; if `None`, `token_env` is read from the
/// environment.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub token_env: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            token: None,
            token_env: "TELEGRAM_BOT_TOKEN".into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TelegramConfigError {
    #[error("telegram: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

impl TelegramConfig {
    /// Returns a valid bot token, reading `token_env` from the env if
    /// `self.token` is `None`.
    pub fn resolve(&self) -> Result<String, TelegramConfigError> {
        if let Some(t) = &self.token {
            return Ok(t.clone());
        }
        match std::env::var(&self.token_env) {
            Ok(v) if !v.is_empty() => Ok(v),
            _ => Err(TelegramConfigError::MissingCredential {
                var: self.token_env.clone(),
            }),
        }
    }

    /// Resolve the token and build a `TelegramAdapter`.
    pub fn build_adapter(&self) -> Result<TelegramAdapter, TelegramConfigError> {
        Ok(TelegramAdapter::new(&self.resolve()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_token() {
        let cfg = TelegramConfig {
            token: Some("explicit".into()),
            token_env: "SHOULD_NOT_BE_READ".into(),
        };
        assert_eq!(cfg.resolve().unwrap(), "explicit");
    }

    #[test]
    fn resolve_falls_back_to_env() {
        // SAFETY: test sets an env var that is not read by any production code
        // path during this test. The name is chosen to be unique.
        unsafe { std::env::set_var("TELEGRAM_TEST_ENV", "from_env") };
        let cfg = TelegramConfig {
            token: None,
            token_env: "TELEGRAM_TEST_ENV".into(),
        };
        assert_eq!(cfg.resolve().unwrap(), "from_env");
        unsafe { std::env::remove_var("TELEGRAM_TEST_ENV") };
    }

    #[test]
    fn resolve_errors_when_neither_set() {
        let cfg = TelegramConfig {
            token: None,
            token_env: "TELEGRAM_NONEXISTENT_VAR_42".into(),
        };
        assert!(matches!(
            cfg.resolve(),
            Err(TelegramConfigError::MissingCredential { .. })
        ));
    }
}
