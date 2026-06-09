use std::path::Path;

use anyhow::{Context, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PerryHermesConfig {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub gateway: GatewayTomlConfig,
}

// ---------------------------------------------------------------------------
// Gateway platform config — owned by hermes-agent because the TOML schema
// is shared with config.toml. hermes-gateway consumes these types from
// `perry_hermes_agent::config` and builds the runtime PlatformAdapters.
// ---------------------------------------------------------------------------

/// `[gateway]` block in config.toml. Each sub-block is optional; `None`
/// means that platform is not enabled.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct GatewayTomlConfig {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub qqbot: Option<QqBotConfig>,
}

/// Telegram adapter config. The `token_env` default is `TELEGRAM_BOT_TOKEN`;
/// `allowed_users` mirrors the `GatewayConfig.allowed_users` map for the
/// `"telegram"` key.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TelegramConfig {
    /// Env var name holding the bot token.
    #[serde(default = "default_telegram_token_env")]
    pub token_env: String,
    /// Optional explicit token (overrides env lookup).
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            token_env: default_telegram_token_env(),
            token: None,
            allowed_users: Vec::new(),
        }
    }
}

fn default_telegram_token_env() -> String {
    "TELEGRAM_BOT_TOKEN".into()
}

/// QQ Bot adapter config. `app_id_env` / `app_secret_env` default to
/// `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QqBotConfig {
    /// Env var name holding the app ID.
    #[serde(default = "default_qqbot_app_id_env")]
    pub app_id_env: String,
    /// Env var name holding the app secret.
    #[serde(default = "default_qqbot_app_secret_env")]
    pub app_secret_env: String,
    /// Optional explicit app ID (overrides env lookup).
    #[serde(default)]
    pub app_id: Option<String>,
    /// Optional explicit app secret.
    #[serde(default)]
    pub app_secret: Option<String>,
    /// Use the QQ sandbox environment.
    #[serde(default)]
    pub sandbox: bool,
    /// Subscribed intent bitmask. 0 means `Intents::PUBLIC_MESSAGES`.
    #[serde(default)]
    pub intents: u32,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            app_id_env: default_qqbot_app_id_env(),
            app_secret_env: default_qqbot_app_secret_env(),
            app_id: None,
            app_secret: None,
            sandbox: false,
            intents: 0,
            allowed_users: Vec::new(),
        }
    }
}

fn default_qqbot_app_id_env() -> String {
    "QQ_BOT_APP_ID".into()
}

fn default_qqbot_app_secret_env() -> String {
    "QQ_BOT_APP_SECRET".into()
}

// ---------------------------------------------------------------------------
// Config errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TelegramConfigError {
    #[error("telegram: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

#[derive(Debug, thiserror::Error)]
pub enum QqBotConfigError {
    #[error("qqbot: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

// ---------------------------------------------------------------------------
// Resolve methods — read env vars with the configured names, falling back
// to explicit values in the config. hermes-gateway consumes these to build
// the runtime PlatformAdapters.
// ---------------------------------------------------------------------------

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
}

fn read_env(var: &str) -> Result<String, QqBotConfigError> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(QqBotConfigError::MissingCredential { var: var.into() }),
    }
}

impl PerryHermesConfig {
    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse config: {}", path.display()))
    }

    pub fn resolve_provider(&self) -> anyhow::Result<ResolvedProviderConfig> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.name == self.agent.default_provider)
            .ok_or_else(|| {
                anyhow!(
                    "agent.default_provider {:?} does not match any [[providers]].name",
                    self.agent.default_provider
                )
            })?;
        let model = provider
            .models
            .iter()
            .find(|model| model.name == self.agent.default_model)
            .ok_or_else(|| {
                anyhow!(
                    "agent.default_model {:?} does not match any model for provider {:?}",
                    self.agent.default_model,
                    provider.name
                )
            })?;

        Ok(ResolvedProviderConfig {
            name: provider.name.clone(),
            kind: provider.kind,
            api_key_env: provider.api_key_env.clone(),
            model: model.name.clone(),
            base_url: provider.base_url.clone(),
            api_key_header: provider.api_key_header.clone(),
            thinking: provider.thinking.clone(),
            context_window_size: model.context_window_size,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub api_key_env: Option<String>,
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_header: Option<String>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub api_key_env: Option<String>,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_header: Option<String>,
    pub thinking: Option<ThinkingConfig>,
    pub context_window_size: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub name: String,
    pub context_window_size: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Echo,
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ThinkingConfig {
    pub mode: ThinkingMode,
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingMode {
    Off,
    Manual,
    Adaptive,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub default_provider: String,
    pub default_model: String,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    /// Enable context compression. Default true; set false to disable.
    #[serde(default = "default_context_compression_enabled")]
    pub context_compression_enabled: bool,
    /// Threshold percentage of model context at which compression triggers.
    /// Default 0.50 (50%).
    #[serde(default)]
    pub context_compression_threshold_percent: Option<f64>,
}

fn default_context_compression_enabled() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: None,
            default_provider: String::new(),
            default_model: String::new(),
            disabled_toolsets: Vec::new(),
            context_compression_enabled: default_context_compression_enabled(),
            context_compression_threshold_percent: None,
        }
    }
}

/// Test fixtures — re-exported from the library root for integration tests.
/// These are unused functions in production builds; they only run during tests.
pub mod test_helpers {
    use super::*;

    impl PerryHermesConfig {
        /// Minimal valid config: single echo provider + agent pointing at
        /// it, no platforms. The "happy path" fixture.
        pub fn for_test_echo() -> Self {
            Self {
                providers: vec![ProviderConfig::for_test_echo()],
                agent: AgentConfig {
                    default_provider: "local".into(),
                    default_model: "echo".into(),
                    ..AgentConfig::default()
                },
                ..Default::default()
            }
        }
    }

    impl ProviderConfig {
        /// Single echo provider — baseline for tests that exercise config
        /// parsing or `resolve_provider`.
        pub fn for_test_echo() -> Self {
            Self {
                name: "local".into(),
                kind: ProviderKind::Echo,
                api_key_env: None,
                models: vec![ModelConfig {
                    name: "echo".into(),
                    context_window_size: 128_000,
                }],
                base_url: None,
                api_key_header: None,
                thinking: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anthropic_provider_config() {
        let input = r#"
[[providers]]
name = "minimax"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[[providers.models]]
name = "mimo-v2.5"
context_window_size = 1_000_000

[providers.thinking]
mode = "adaptive"
display = "summarized"
effort = "medium"

[agent]
default_provider = "minimax"
default_model = "mimo-v2.5"
max_iterations = 12
disabled_toolsets = ["terminal"]
"#;

        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        let provider = &config.providers[0];

        assert_eq!(provider.name, "minimax");
        assert_eq!(provider.kind, ProviderKind::Anthropic);
        assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.xiaomimimo.com/anthropic/v1")
        );
        assert_eq!(provider.api_key_header.as_deref(), Some("api-key"));
        assert_eq!(provider.models[0].name, "mimo-v2.5");
        assert_eq!(provider.models[0].context_window_size, 1_000_000);
        let thinking = provider.thinking.clone().unwrap();
        assert_eq!(thinking.mode, ThinkingMode::Adaptive);
        assert_eq!(thinking.display.as_deref(), Some("summarized"));
        assert_eq!(thinking.effort.as_deref(), Some("medium"));
        assert_eq!(config.agent.default_provider, "minimax");
        assert_eq!(config.agent.default_model, "mimo-v2.5");
        assert_eq!(config.agent.max_iterations, Some(12));
        assert_eq!(config.agent.disabled_toolsets, vec!["terminal"]);
    }

    #[test]
    fn agent_and_skills_default_when_omitted() {
        let input = r#"
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();

        assert_eq!(config.providers[0].kind, ProviderKind::Echo);
        assert!(config.agent.context_compression_enabled);
    }

    #[test]
    fn parses_context_compression_config() {
        let input = r#"
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
context_compression_enabled = true
context_compression_threshold_percent = 0.60
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        assert!(config.agent.context_compression_enabled);
        assert_eq!(
            config.agent.context_compression_threshold_percent,
            Some(0.60)
        );
    }

    #[test]
    fn parses_explicitly_disabled_context_compression_config() {
        let input = r#"
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
context_compression_enabled = false
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        assert!(!config.agent.context_compression_enabled);
    }

    #[test]
    fn model_context_window_size_round_trips() {
        let input = r#"
[[providers]]
name = "openai-main"
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[[providers.models]]
name = "gpt-4.1"
context_window_size = 200_000

[agent]
default_provider = "openai-main"
default_model = "gpt-4.1"
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        assert_eq!(config.providers[0].models[0].context_window_size, 200_000);
    }

    #[test]
    fn model_context_window_size_is_required() {
        let input = r#"
[[providers]]
name = "openai-main"
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[[providers.models]]
name = "gpt-4.1"

[agent]
default_provider = "openai-main"
default_model = "gpt-4.1"
"#;
        let err = toml::from_str::<PerryHermesConfig>(input).unwrap_err();
        assert!(err.to_string().contains("context_window_size"));
    }

    #[test]
    fn resolves_default_provider_and_model() {
        let input = r#"
[[providers]]
name = "minimax"
kind = "anthropic"
api_key_env = "MINIMAX_API_KEY"
base_url = "https://api.minimaxi.com/anthropic/v1"

[[providers.models]]
name = "MiniMax-M3"
context_window_size = 1_000_000

[[providers.models]]
name = "MiniMax-M2.7"
context_window_size = 204_800

[[providers]]
name = "openai-main"
kind = "openai"
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"

[[providers.models]]
name = "gpt-4.1"
context_window_size = 1_047_576

[agent]
default_provider = "minimax"
default_model = "MiniMax-M2.7"
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        let selected = config.resolve_provider().unwrap();

        assert_eq!(selected.name, "minimax");
        assert_eq!(selected.kind, ProviderKind::Anthropic);
        assert_eq!(selected.model, "MiniMax-M2.7");
        assert_eq!(selected.context_window_size, 204_800);
        assert_eq!(
            selected.base_url.as_deref(),
            Some("https://api.minimaxi.com/anthropic/v1")
        );
    }

    #[test]
    fn resolve_provider_errors_when_default_model_is_not_on_provider() {
        let input = r#"
[[providers]]
name = "minimax"
kind = "anthropic"
api_key_env = "MINIMAX_API_KEY"
base_url = "https://api.minimaxi.com/anthropic/v1"

[[providers.models]]
name = "MiniMax-M3"
context_window_size = 1_000_000

[agent]
default_provider = "minimax"
default_model = "missing-model"
"#;
        let config: PerryHermesConfig = toml::from_str(input).unwrap();
        let err = config.resolve_provider().unwrap_err().to_string();

        assert!(err.contains("missing-model"), "{err}");
        assert!(err.contains("minimax"), "{err}");
    }
}
