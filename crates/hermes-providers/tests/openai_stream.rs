//! Integration test for OpenAiProvider::stream using a raw TcpListener
//! to serve canned SSE bytes (httpmock doesn't expose captured bodies
//! per CLAUDE.md).

use std::time::Duration;
use futures::StreamExt;
use hermes_core::provider::{FinishReason, Provider};
use hermes_providers::OpenAiProvider;
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
        let _ = tokio::time::timeout(Duration::from_millis(500), socket.read(&mut buf)).await;
        // Respond with SSE
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(SAMPLE_BODY).await.unwrap();
        socket.flush().await.unwrap();
        // Keep socket open briefly so the client can drain
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let provider = OpenAiProvider::new("test-key", "gpt-test")
        .with_base_url(format!("http://{addr}"));
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