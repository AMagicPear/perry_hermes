use futures::StreamExt;
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{FinishReason, Provider};
use hermes_core::registry::ToolSchema;
use hermes_core::ProviderError;
use hermes_providers::AnthropicProvider;
use httpmock::prelude::*;
use tokio_util::sync::CancellationToken;

fn message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: Content::Text(text.into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

fn tool_schema() -> ToolSchema {
    ToolSchema {
        name: "bash".into(),
        description: "Run a shell command".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }),
    }
}

#[tokio::test]
async fn anthropic_provider_posts_messages_request_with_headers_and_tools() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", "test-key")
                .header("anthropic-version", "2023-06-01")
                .matches(|req| {
                    let Some(body) = &req.body else {
                        return false;
                    };
                    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
                        return false;
                    };
                    json["model"] == "claude-sonnet-4-5"
                        && json["system"] == "system prompt"
                        && json["tool_choice"] == serde_json::json!({ "type": "auto" })
                        && json["tools"][0]["input_schema"]["type"] == "object"
                        && json["messages"][0]["role"] == "user"
                });
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        })
        .await;

    let provider =
        AnthropicProvider::new("test-key", "claude-sonnet-4-5").with_base_url(server.url("/v1"));
    let mut stream = provider
        .stream(
            &[
                message(Role::System, "system prompt"),
                message(Role::User, "hi"),
            ],
            &[tool_schema()],
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let mut final_reason = None;
    while let Some(delta) = stream.next().await {
        final_reason = delta.unwrap().finish_reason.or(final_reason);
    }

    mock.assert_async().await;
    assert_eq!(final_reason, Some(FinishReason::Stop));
}

#[tokio::test]
async fn anthropic_provider_maps_401_and_429() {
    let server = MockServer::start_async().await;
    let auth = server
        .mock_async(|when, then| {
            when.method(POST).path("/auth/messages");
            then.status(401).body("invalid api key");
        })
        .await;
    let rate = server
        .mock_async(|when, then| {
            when.method(POST).path("/rate/messages");
            then.status(429).body("slow down");
        })
        .await;

    let auth_provider =
        AnthropicProvider::new("bad", "claude-sonnet-4-5").with_base_url(server.url("/auth"));
    let err = match auth_provider
        .stream(&[message(Role::User, "hi")], &[], CancellationToken::new())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("expected auth error"),
    };
    assert!(matches!(err, ProviderError::Auth(_)));
    auth.assert_async().await;

    let rate_provider =
        AnthropicProvider::new("k", "claude-sonnet-4-5").with_base_url(server.url("/rate"));
    let err = match rate_provider
        .stream(&[message(Role::User, "hi")], &[], CancellationToken::new())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("expected rate limit error"),
    };
    assert!(matches!(err, ProviderError::RateLimited { .. }));
    rate.assert_async().await;
}

#[tokio::test]
async fn anthropic_provider_can_use_custom_api_key_header() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("api-key", "test-key");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n");
        })
        .await;

    let provider = AnthropicProvider::new("test-key", "mimo-v2.5")
        .with_base_url(server.url("/v1"))
        .with_api_key_header("api-key");
    let _stream = provider
        .stream(&[message(Role::User, "hi")], &[], CancellationToken::new())
        .await
        .unwrap();

    mock.assert_async().await;
}

#[tokio::test]
async fn anthropic_provider_streams_tool_use_deltas() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"bash\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"ls\\\"}\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        })
        .await;

    let provider =
        AnthropicProvider::new("test-key", "claude-sonnet-4-5").with_base_url(server.url("/v1"));
    let completion = provider
        .complete(
            &[message(Role::User, "run ls")],
            &[tool_schema()],
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(completion.finish_reason, FinishReason::ToolUse);
    let calls = completion.message.tool_calls.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].name, "bash");
    assert_eq!(calls[0].arguments, serde_json::json!({ "command": "ls" }));
    assert_eq!(completion.usage.input_tokens, 10);
    assert_eq!(completion.usage.output_tokens, 9);
}

#[tokio::test]
async fn anthropic_provider_serializes_prior_tool_results_as_user_tool_result_blocks() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .body_contains(r#""type":"tool_use""#)
                .body_contains(r#""tool_use_id":"toolu_1""#)
                .body_contains(r#""content":"done""#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n");
        })
        .await;

    let assistant = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: Some(vec![ToolCall {
            id: "toolu_1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "echo done" }),
        }]),
    };
    let tool = Message {
        role: Role::Tool,
        content: Content::Text("done".into()),
        reasoning: None,
        tool_call_id: Some("toolu_1".into()),
        tool_calls: None,
    };

    let provider =
        AnthropicProvider::new("test-key", "claude-sonnet-4-5").with_base_url(server.url("/v1"));
    let _stream = provider
        .stream(
            &[message(Role::User, "run"), assistant, tool],
            &[tool_schema()],
            CancellationToken::new(),
        )
        .await
        .unwrap();

    mock.assert_async().await;
}
