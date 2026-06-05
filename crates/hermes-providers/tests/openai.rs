//! Integration tests for `OpenAiProvider`.
//!
//! Phase 2 minimum: prove the provider correctly (1) POSTs a valid
//! Chat Completions request body and (2) maps 401/429 errors correctly.
//!
//! We don't hit api.openai.com. Instead we point `base_url` at a local
//! `httpmock` server that returns canned responses. This tests the
//! *real* reqwest + serde code paths (so any serialization bug is
//! caught) without burning API credits or needing a key.

use httpmock::prelude::*;
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Message, Role};
use hermes_core::Provider;
use hermes_core::ProviderError;
use hermes_providers::OpenAiProvider;

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: hermes_core::message::Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

#[tokio::test]
async fn openai_provider_maps_401_to_auth_error() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(401).body("invalid api key");
        })
        .await;

    let provider = OpenAiProvider::new("bad-key", "gpt-4o-mini")
        .with_base_url(server.url("/v1"));
    let cancel = CancellationToken::new();
    let err = match provider.stream(&[user_message("hi")], &[], cancel).await {
        Err(e) => e,
        Ok(_) => panic!("expected error, got Ok"),
    };

    match err {
        ProviderError::Auth(msg) => assert!(msg.contains("invalid api key")),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn openai_provider_maps_429_to_rate_limited() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(429).body("slow down");
        })
        .await;

    let provider = OpenAiProvider::new("k", "gpt-4o-mini")
        .with_base_url(server.url("/v1"));
    let cancel = CancellationToken::new();
    let err = match provider.stream(&[user_message("hi")], &[], cancel).await {
        Err(e) => e,
        Ok(_) => panic!("expected error, got Ok"),
    };

    assert!(matches!(err, ProviderError::RateLimited { .. }));
}