//! Round-trip test: when the agent loop sends a second request, the
//! assistant message's `tool_calls` must be present in the request
//! body. Otherwise the LLM has no memory of which tool it called, and
//! the tool-result `role: tool` message comes out of nowhere.
//!
//! Phase 3 closes the "TODO(phase 3): round-trip assistant
//! `tool_calls`" comment in `openai.rs`.
//!
//! httpmock 0.7 doesn't expose a clean way to inspect request bodies
//! across multiple calls, so we run a tiny `tokio::net::TcpListener`
//! that records every POST body's bytes and replies with a canned
//! Chat Completions JSON. Same protocol surface as a real OpenAI-
//! compatible endpoint, no extra deps.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use perry_hermes_core::message::{Content, ContentPart, Message, Role, ToolCall};
use perry_hermes_core::provider::Provider;
use perry_hermes_providers::OpenAiProvider;

/// Minimal HTTP/1.1 server that records POST bodies and returns the
/// same canned response every time. Parses `Content-Length` to know
/// when the request body is done.
async fn run_capturing_server(listener: TcpListener, bodies: Arc<Mutex<Vec<String>>>) {
    let canned = serde_json::to_string(&json!({
        "id": "x", "model": "m",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
    }))
    .unwrap();

    loop {
        let Ok((mut socket, _addr)) = listener.accept().await else {
            break;
        };
        let bodies = Arc::clone(&bodies);
        let canned = canned.clone();
        tokio::spawn(async move {
            // Read the request head until \r\n\r\n to extract headers.
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = match socket.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 32 * 1024 {
                    return; // way too big, bail
                }
            }
            // Find Content-Length.
            let head = String::from_utf8_lossy(&buf);
            let content_length: usize = head
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            // Find the index of \r\n\r\n.
            let split = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
                Some(p) => p + 4,
                None => return,
            };
            let mut body = buf[split..].to_vec();
            while body.len() < content_length {
                let n = match socket.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                body.extend_from_slice(&tmp[..n]);
            }
            let body_str = String::from_utf8_lossy(&body).to_string();
            bodies.lock().unwrap().push(body_str);

            // Reply.
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                canned.len(),
                canned
            );
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
}

#[tokio::test]
async fn openai_provider_serializes_assistant_tool_calls_in_request_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_for_server = Arc::clone(&bodies);
    tokio::spawn(async move {
        run_capturing_server(listener, bodies_for_server).await;
    });

    let base_url = format!("http://{}", addr);
    let provider = OpenAiProvider::new("k", "m").with_base_url(&base_url);
    let cancel = CancellationToken::new();

    // Call 1: just a user message — no tool_calls.
    provider
        .complete(
            &[Message {
                role: Role::User,
                content: Content::Text("do something".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            &[],
            cancel.clone(),
        )
        .await
        .expect("call 1");

    // Call 2: assistant carries a tool_call with a unique marker id.
    let marker = "MARKER_roundtrip_call_42";
    provider
        .complete(
            &[
                Message {
                    role: Role::User,
                    content: Content::Text("do something".into()),
                    reasoning: None,
                    tool_call_id: None,
                    tool_calls: None,
                },
                Message {
                    role: Role::Assistant,
                    content: Content::Text(String::new()),
                    reasoning: None,
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: marker.into(),
                        name: "bash".into(),
                        arguments: json!({ "command": "ls" }),
                    }]),
                },
            ],
            &[],
            cancel,
        )
        .await
        .expect("call 2");

    // Give the server a moment to finish recording.
    for _ in 0..20 {
        if bodies.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let captured = bodies.lock().unwrap().clone();
    assert_eq!(
        captured.len(),
        2,
        "expected exactly 2 captured request bodies, got {}",
        captured.len()
    );

    // Call 1's body must NOT contain the marker.
    assert!(
        !captured[0].contains(marker),
        "first request body should not contain tool_call marker, got: {}",
        captured[0]
    );

    // Call 2's body MUST contain the marker (proves tool_calls is
    // being serialized in the request body).
    assert!(
        captured[1].contains(marker),
        "second request body should contain tool_call marker, got: {}",
        captured[1]
    );
}

#[tokio::test]
async fn openai_provider_serializes_content_parts_as_content_array() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_for_server = Arc::clone(&bodies);
    tokio::spawn(async move {
        run_capturing_server(listener, bodies_for_server).await;
    });

    let base_url = format!("http://{}", addr);
    let provider = OpenAiProvider::new("k", "m").with_base_url(&base_url);

    provider
        .complete(
            &[Message {
                role: Role::User,
                content: Content::Parts(vec![
                    ContentPart::Text {
                        text: "first text".into(),
                    },
                    ContentPart::ImageUrl {
                        url: "https://example.com/image.png".into(),
                    },
                    ContentPart::Text {
                        text: "second text".into(),
                    },
                ]),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            &[],
            CancellationToken::new(),
        )
        .await
        .expect("call");

    for _ in 0..20 {
        if bodies.lock().unwrap().len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let captured = bodies.lock().unwrap().clone();
    assert_eq!(
        captured.len(),
        1,
        "expected exactly 1 captured request body, got {}",
        captured.len()
    );

    let body: serde_json::Value = serde_json::from_str(&captured[0]).unwrap();
    let content = &body["messages"][0]["content"];
    assert!(
        content.is_array(),
        "content parts should be sent as a content array, got: {}",
        content
    );
    assert_eq!(content[0], json!({ "type": "text", "text": "first text" }));
    assert_eq!(
        content[1],
        json!({ "type": "image_url", "image_url": { "url": "https://example.com/image.png" } })
    );
    assert_eq!(content[2], json!({ "type": "text", "text": "second text" }));
}
