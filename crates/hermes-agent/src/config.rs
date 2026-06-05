use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Echo,
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ThinkingConfig {
    pub mode: ThinkingMode,
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingMode {
    Off,
    Manual,
    Adaptive,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
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
    }
}
