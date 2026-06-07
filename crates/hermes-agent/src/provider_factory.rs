use anyhow::{anyhow, Context};
use perry_hermes_core::Provider;
use perry_hermes_providers::{
    AnthropicProvider, AnthropicRequestOptions, AnthropicThinking, EchoProvider, OpenAiProvider,
};

use crate::config::{ProviderKind, ResolvedProviderConfig, ThinkingConfig, ThinkingMode};

pub fn build_provider(config: &ResolvedProviderConfig) -> anyhow::Result<Box<dyn Provider>> {
    match config.kind {
        ProviderKind::Echo => Ok(Box::new(EchoProvider::new())),

        ProviderKind::Openai => {
            let model = config.model.as_str();
            let base_url = config.base_url.as_deref().ok_or_else(|| {
                anyhow!(
                    "provider {:?} base_url is required for kind=openai",
                    config.name
                )
            })?;
            let api_key_env = config.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            let api_key = std::env::var(api_key_env).with_context(|| {
                format!(
                    "{api_key_env} is not set. Export it or set api_key_env on provider {:?}.",
                    config.name
                )
            })?;
            Ok(Box::new(
                OpenAiProvider::new(api_key, model).with_base_url(base_url),
            ))
        }

        ProviderKind::Anthropic => {
            let model = config.model.as_str();
            let base_url = config.base_url.as_deref().ok_or_else(|| {
                anyhow!(
                    "provider {:?} base_url is required for kind=anthropic",
                    config.name
                )
            })?;
            let api_key_env = config.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY");
            let api_key = std::env::var(api_key_env).with_context(|| {
                format!(
                    "{api_key_env} is not set. Export it or set api_key_env on provider {:?}.",
                    config.name
                )
            })?;
            let api_key_header = config
                .api_key_header
                .clone()
                .unwrap_or_else(|| "x-api-key".into());
            let request_options = anthropic_request_options(config.thinking.as_ref());
            Ok(Box::new(
                AnthropicProvider::new(api_key, model)
                    .with_base_url(base_url)
                    .with_api_key_header(api_key_header)
                    .with_request_options(request_options),
            ))
        }
    }
}

fn anthropic_request_options(thinking: Option<&ThinkingConfig>) -> AnthropicRequestOptions {
    let resolved = thinking.and_then(|t| match t.mode {
        ThinkingMode::Off => None,
        ThinkingMode::Manual => Some(AnthropicThinking::Manual {
            budget_tokens: t.budget_tokens.unwrap_or(8_000),
        }),
        ThinkingMode::Adaptive => Some(AnthropicThinking::Adaptive {
            display: t.display.clone().unwrap_or_else(|| "summarized".into()),
            effort: t.effort.clone(),
        }),
    });
    AnthropicRequestOptions { thinking: resolved }
}
