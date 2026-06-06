use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct HermesConfig {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

impl HermesConfig {
    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse config: {}", path.display()))
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ProviderConfig {
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_header: Option<String>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
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
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Enable context compression. Default true; set false to disable.
    #[serde(default = "default_context_compression_enabled")]
    pub context_compression_enabled: bool,
    /// Threshold percentage of model context at which compression triggers.
    /// Default 0.50 (50%).
    #[serde(default)]
    pub context_compression_threshold_percent: Option<f64>,
    /// Total context window in tokens for the configured model. Used as the
    /// denominator for `context_compression_threshold_percent`, and rendered
    /// as the "24.2K / 200K [gauge]" segment in the TUI status bar. When
    /// `None`, the compressor falls back to 128_000 and the TUI hides the
    /// context segment.
    #[serde(default)]
    pub context_window_size: Option<u64>,
}

fn default_context_compression_enabled() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: None,
            disabled_toolsets: Vec::new(),
            system_prompt: None,
            context_compression_enabled: default_context_compression_enabled(),
            context_compression_threshold_percent: None,
            context_window_size: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anthropic_provider_config() {
        let input = r#"
[provider]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "mimo-v2.5"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[provider.thinking]
mode = "adaptive"
display = "summarized"
effort = "medium"

[agent]
max_iterations = 12
disabled_toolsets = ["terminal"]
"#;

        let config: HermesConfig = toml::from_str(input).unwrap();

        assert_eq!(config.provider.kind, ProviderKind::Anthropic);
        assert_eq!(
            config.provider.api_key_env.as_deref(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(config.provider.model.as_deref(), Some("mimo-v2.5"));
        assert_eq!(
            config.provider.base_url.as_deref(),
            Some("https://api.xiaomimimo.com/anthropic/v1")
        );
        assert_eq!(config.provider.api_key_header.as_deref(), Some("api-key"));
        let thinking = config.provider.thinking.unwrap();
        assert_eq!(thinking.mode, ThinkingMode::Adaptive);
        assert_eq!(thinking.display.as_deref(), Some("summarized"));
        assert_eq!(thinking.effort.as_deref(), Some("medium"));
        assert_eq!(config.agent.max_iterations, Some(12));
        assert_eq!(config.agent.disabled_toolsets, vec!["terminal"]);
    }

    #[test]
    fn agent_and_skills_default_when_omitted() {
        let input = r#"
[provider]
kind = "echo"
"#;
        let config: HermesConfig = toml::from_str(input).unwrap();

        assert_eq!(config.provider.kind, ProviderKind::Echo);
        assert_eq!(config.agent, AgentConfig::default());
        assert!(config.agent.context_compression_enabled);
    }

    #[test]
    fn parses_context_compression_config() {
        let input = r#"
[provider]
kind = "echo"

[agent]
context_compression_enabled = true
context_compression_threshold_percent = 0.60
"#;
        let config: HermesConfig = toml::from_str(input).unwrap();
        assert!(config.agent.context_compression_enabled);
        assert_eq!(
            config.agent.context_compression_threshold_percent,
            Some(0.60)
        );
    }

    #[test]
    fn parses_explicitly_disabled_context_compression_config() {
        let input = r#"
[provider]
kind = "echo"

[agent]
context_compression_enabled = false
"#;
        let config: HermesConfig = toml::from_str(input).unwrap();
        assert!(!config.agent.context_compression_enabled);
    }

    #[test]
    fn context_window_size_round_trips() {
        let input = r#"
[provider]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1"

[agent]
context_window_size = 200_000
"#;
        let config: HermesConfig = toml::from_str(input).unwrap();
        assert_eq!(config.agent.context_window_size, Some(200_000));
    }

    #[test]
    fn context_window_size_absent_defaults_to_none() {
        let input = r#"
[provider]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1"
"#;
        let config: HermesConfig = toml::from_str(input).unwrap();
        assert_eq!(config.agent.context_window_size, None);
    }
}
