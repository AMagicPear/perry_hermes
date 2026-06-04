//! Integration tests for `OpenAiProvider`.
//!
//! Phase 2 minimum: prove the provider correctly (1) POSTs a valid
//! Chat Completions request body, (2) parses a normal `stop` response,
//! and (3) maps finish_reason strings to our `FinishReason` enum.
//!
//! We don't hit api.openai.com. Instead we point `base_url` at a local
//! `httpmock` server that returns canned responses. This tests the
//! *real* reqwest + serde code paths (so any serialization bug is
//! caught) without burning API credits or needing a key.

use std::time::Duration;

use httpmock::prelude::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::{FinishReason, Provider};
use hermes_core::ProviderError;
use hermes_providers::OpenAiProvider;

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

#[tokio::test]
async fn openai_provider_parses_stop_response() {
    // ── Arrange: mock server that mimics OpenAI's Chat Completions ──
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "model": "gpt-4o-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": "hi there",
                            },
                            "finish_reason": "stop",
                        }],
                        "usage": {
                            "prompt_tokens": 12,
                            "completion_tokens": 4,
                            "total_tokens": 16,
                        },
                    })
                    .to_string(),
                );
        })
        .await;

    // ── Act: call the provider pointing at the mock server ──
    let provider = OpenAiProvider::new("test-key", "gpt-4o-mini")
        .with_base_url(server.url("/v1"));
    let cancel = CancellationToken::new();
    let completion = tokio::time::timeout(
        Duration::from_secs(5),
        provider.complete(&[user_message("hello")], &[], cancel),
    )
    .await
    .expect("provider should not hang")
    .expect("provider should return Ok");

    // ── Assert: response was parsed correctly ──
    mock.assert_async().await;

    assert_eq!(completion.finish_reason, FinishReason::Stop);
    let text = match &completion.message.content {
        Content::Text(s) => s.clone(),
        Content::Parts(_) => panic!("expected text content"),
    };
    assert_eq!(text, "hi there");
    assert_eq!(completion.message.role, Role::Assistant);
    assert_eq!(completion.usage.input_tokens, 12);
    assert_eq!(completion.usage.output_tokens, 4);
    assert!(completion.message.tool_calls.is_none());
    assert_eq!(provider.name(), "openai");
    assert_eq!(provider.model(), "gpt-4o-mini");
}

#[tokio::test]
async fn openai_provider_parses_tool_calls() {
    // OpenAI's `arguments` field is a JSON *string* (not an object).
    // The provider must `serde_json::from_str` it; the test would fail
    // loudly with a Null/garbage value otherwise.
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    json!({
                        "id": "chatcmpl-tc",
                        "model": "gpt-4o-mini",
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": null,
                                "tool_calls": [{
                                    "id": "call_abc",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"city\":\"sf\"}",
                                    }
                                }]
                            },
                            "finish_reason": "tool_calls",
                        }],
                        "usage": {
                            "prompt_tokens": 20,
                            "completion_tokens": 5,
                            "total_tokens": 25,
                        },
                    })
                    .to_string(),
                );
        })
        .await;

    let provider = OpenAiProvider::new("test-key", "gpt-4o-mini")
        .with_base_url(server.url("/v1"));
    let cancel = CancellationToken::new();
    let completion = tokio::time::timeout(
        Duration::from_secs(5),
        provider.complete(&[user_message("weather?")], &[], cancel),
    )
    .await
    .expect("provider should not hang")
    .expect("provider should return Ok");

    mock.assert_async().await;

    assert_eq!(completion.finish_reason, FinishReason::ToolUse);
    let calls = completion
        .message
        .tool_calls
        .as_ref()
        .expect("expected tool_calls to be set");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_abc");
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].arguments, json!({"city": "sf"}));
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
    let err = provider
        .complete(&[user_message("hi")], &[], cancel)
        .await
        .expect_err("should fail");

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
    let err = provider
        .complete(&[user_message("hi")], &[], cancel)
        .await
        .expect_err("should fail");

    assert!(matches!(err, ProviderError::RateLimited { .. }));
}
