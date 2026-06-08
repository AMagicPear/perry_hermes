//! Integration test for OpenAiProvider::stream using a raw TcpListener
//! to serve canned SSE bytes (httpmock doesn't expose captured bodies
//! per AGENTS.md).

use futures::StreamExt;
use perry_hermes_core::ProviderError;
use perry_hermes_core::provider::{FinishReason, Provider};
use perry_hermes_providers::OpenAiProvider;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const SAMPLE_BODY: &[u8] = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

#[tokio::test]
async fn stream_parses_sse_chunks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request (don't bother parsing — just consume it)
        let mut buf = [0u8; 4096];
        let n = tokio::time::timeout(Duration::from_millis(500), socket.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
        assert!(!request.contains("accept-encoding:"));
        // Respond with SSE
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(SAMPLE_BODY).await.unwrap();
        socket.flush().await.unwrap();
        // Keep socket open briefly so the client can drain
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let provider =
        OpenAiProvider::new("test-key", "gpt-test").with_base_url(format!("http://{addr}"));
    let cancel = CancellationToken::new();
    let messages = vec![];
    let tools = vec![];

    let mut stream = provider.stream(&messages, &tools, cancel).await.unwrap();
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.unwrap());
    }
    server.await.unwrap();

    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
    assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
}

#[tokio::test]
async fn stream_error_preserves_body_error_source() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_millis(500), socket.read(&mut buf)).await;
        let response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Content-Length: 1000\r\n",
            "\r\n"
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket
            .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n")
            .await
            .unwrap();
    });

    let provider =
        OpenAiProvider::new("test-key", "gpt-test").with_base_url(format!("http://{addr}"));
    let cancel = CancellationToken::new();
    let mut stream = provider.stream(&[], &[], cancel).await.unwrap();

    let first = stream
        .next()
        .await
        .expect("partial chunk")
        .expect("partial chunk should parse");
    assert_eq!(first.content_delta.as_deref(), Some("partial"));

    let err = stream.next().await.expect("body error").unwrap_err();
    server.await.unwrap();

    match err {
        ProviderError::Transport(message) => {
            assert!(message.contains("error decoding response body"));
            assert!(
                message.contains("end of file")
                    || message.contains("body")
                    || message.contains("connection"),
                "expected source chain in transport error, got: {message}"
            );
        }
        other => panic!("expected transport error, got {other:?}"),
    }
}
