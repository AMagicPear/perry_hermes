//! Shared test helpers for integration tests under `tests/`.
//!
//! Integration tests in Rust are compiled as separate binaries and cannot
//! see `#[cfg(test)]` items in the library crate. This module re-declares
//! the small set of factory functions we need.
//!
//! Each integration test that wants to use these helpers must add:
//! ```ignore
//! mod common;
//! use common::*;
//! ```

use perry_hermes_agent::{
    AgentConfig, ModelConfig, PerryHermesConfig, ProviderConfig, ProviderKind,
};

pub fn for_test_echo() -> PerryHermesConfig {
    PerryHermesConfig {
        providers: vec![for_test_provider_echo()],
        agent: AgentConfig {
            default_provider: "local".into(),
            default_model: "echo".into(),
            ..AgentConfig::default()
        },
        ..Default::default()
    }
}

pub fn for_test_provider_echo() -> ProviderConfig {
    ProviderConfig {
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
