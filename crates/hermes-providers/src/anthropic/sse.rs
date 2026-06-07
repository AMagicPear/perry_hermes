//! SSE parser for the Anthropic Messages API stream.
//!
//! Anthropic's stream format is event-based: each `event: <type>` line
//! names the event, and a `data: {json}` line carries the payload. The
//! parser collects per-stream state (running usage counters) and yields
//! one `CompletionDelta` per non-control event.

use bytes::Bytes;
use futures::{Stream, StreamExt};

use hermes_core::provider::{CompletionDelta, FinishReason, ToolCallDelta};
use hermes_core::{ProviderError, Usage};

/// Strip the `data: ` prefix and surrounding whitespace; return the
/// payload, or `None` if the line is a comment or a control line.
pub(super) fn parse_sse_data_line(line: &str) -> Option<&str> {
    line.strip_prefix("data: ").map(str::trim)
}

/// Per-stream state carried across delta yields.
#[derive(Default)]
pub(super) struct AnthropicStreamState {
    /// Running usage counters. `message_start` initializes from the
    /// API's first usage block; `message_delta` may update it.
    pub usage: Usage,
}

pub(super) fn parse_sse_chunks(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut state = AnthropicStreamState::default();
        let mut bytes = Box::pin(bytes);
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => buffer.push_str(&String::from_utf8_lossy(&c)),
                Err(e) => { yield Err(ProviderError::Transport(e.to_string())); return; }
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                let payload = event
                    .lines()
                    .filter_map(parse_sse_data_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                if payload.is_empty() {
                    continue;
                }
                match parse_sse_data_payload(&payload, &mut state) {
                    Ok(Some(delta)) => yield Ok(delta),
                    Ok(None) => {}
                    Err(e) => { yield Err(e); return; }
                }
            }
        }
    }
}

fn parse_sse_data_payload(
    payload: &str,
    state: &mut AnthropicStreamState,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| ProviderError::InvalidResponse(format!("sse json: {e}")))?;
    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProviderError::InvalidResponse("missing Anthropic SSE type".into()))?;

    match event_type {
        "message_start" => {
            if let Some(usage) = value.get("message").and_then(|m| m.get("usage")) {
                state.usage.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default();
                state.usage.output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default();
                state.usage.cached_input_tokens = cached_input_tokens_from_anthropic_usage(usage);
            }
            Ok(Some(usage_delta(state.usage)))
        }
        "content_block_start" => parse_content_block_start(&value),
        "content_block_delta" => parse_content_block_delta(&value),
        "content_block_stop" | "message_stop" | "ping" => Ok(None),
        "message_delta" => {
            if let Some(usage) = value.get("usage") {
                if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    state.usage.input_tokens = input;
                }
                if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    state.usage.output_tokens = output;
                }
                let cached = cached_input_tokens_from_anthropic_usage(usage);
                if cached > 0 {
                    state.usage.cached_input_tokens = cached;
                }
            }
            let finish_reason = value
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(anthropic_finish_reason);
            Ok(Some(CompletionDelta {
                content_delta: None,
                reasoning_delta: None,
                tool_call_delta: None,
                usage: Some(state.usage),
                finish_reason,
            }))
        }
        "error" => {
            let msg = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Anthropic stream error");
            Err(ProviderError::InvalidResponse(msg.into()))
        }
        other => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic SSE event: {other}"
        ))),
    }
}

fn parse_content_block_start(
    value: &serde_json::Value,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let block = value
        .get("content_block")
        .ok_or_else(|| ProviderError::InvalidResponse("missing content_block".into()))?;
    match block.get("type").and_then(|v| v.as_str()) {
        Some("tool_use") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: value
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize,
                id: block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
                name: block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
                arguments_fragment: None,
            }),
            usage: None,
            finish_reason: None,
        })),
        Some("text" | "thinking") => Ok(None),
        Some(other) => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic content block: {other}"
        ))),
        None => Err(ProviderError::InvalidResponse(
            "missing content_block type".into(),
        )),
    }
}

fn parse_content_block_delta(
    value: &serde_json::Value,
) -> Result<Option<CompletionDelta>, ProviderError> {
    let delta = value
        .get("delta")
        .ok_or_else(|| ProviderError::InvalidResponse("missing content_block delta".into()))?;
    match delta.get("type").and_then(|v| v.as_str()) {
        Some("text_delta") => Ok(Some(CompletionDelta {
            content_delta: delta
                .get("text")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
            reasoning_delta: None,
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })),
        Some("input_json_delta") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index: value
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize,
                id: None,
                name: None,
                arguments_fragment: delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
            }),
            usage: None,
            finish_reason: None,
        })),
        Some("thinking_delta") => Ok(Some(CompletionDelta {
            content_delta: None,
            reasoning_delta: delta
                .get("thinking")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
            tool_call_delta: None,
            usage: None,
            finish_reason: None,
        })),
        Some("signature_delta") => Ok(None),
        Some(other) => Err(ProviderError::InvalidResponse(format!(
            "unknown Anthropic delta: {other}"
        ))),
        None => Err(ProviderError::InvalidResponse(
            "missing content_block delta type".into(),
        )),
    }
}

fn usage_delta(usage: Usage) -> CompletionDelta {
    CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(usage),
        finish_reason: None,
    }
}

fn cached_input_tokens_from_anthropic_usage(usage: &serde_json::Value) -> u64 {
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    cache_read + cache_creation
}

fn anthropic_finish_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(input: &[u8]) -> Vec<CompletionDelta> {
        use futures::stream;
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(input),
        )]));
        futures::executor::block_on(async move {
            let mut out = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                out.push(item.unwrap());
            }
            out
        })
    }

    #[test]
    fn parses_text_tool_and_usage_stream() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n";
        let deltas = drive(input);

        assert_eq!(deltas[0].usage.unwrap().input_tokens, 2);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Hi"));
        assert_eq!(deltas[2].usage.unwrap().output_tokens, 4);
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn message_delta_usage_can_update_input_tokens() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":8,\"output_tokens\":4}}\n\n";
        let deltas = drive(input);

        let usage = deltas[1].usage.unwrap();
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 4);
    }

    #[test]
    fn chunks_split_across_frames_assemble_correctly() {
        use futures::stream;
        let chunks: Vec<&[u8]> = vec![
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
            b"\n",
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        ];
        let byte_stream = stream::iter(
            chunks
                .iter()
                .map(|c| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let s = parse_sse_chunks(byte_stream);
        let deltas: Vec<CompletionDelta> = futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item.unwrap());
            }
            v
        });
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
    }

    #[test]
    fn message_stop_event_terminates_cleanly() {
        // Anthropic uses explicit event types: message_delta carries
        // stop_reason + usage, then message_stop is the actual end
        // marker. The parser must yield the message_delta delta and
        // then return cleanly when message_stop arrives (no extra
        // Ok(None) leak).
        let input = b"\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n\
";
        let deltas = drive(input);

        // Expect: message_start yields usage delta, content_block_delta
        // yields text delta, message_delta yields finish_reason + usage.
        // message_stop is silently consumed and yields nothing extra.
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].usage.unwrap().input_tokens, 2);
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Hi"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
        assert_eq!(deltas[2].usage.unwrap().output_tokens, 4);
    }

    #[test]
    fn partial_utf8_in_a_chunk_does_not_panic() {
        use futures::stream;
        let s = parse_sse_chunks(stream::iter(vec![Ok::<_, reqwest::Error>(
            Bytes::copy_from_slice(b"data: \xFF\xFE\n\n"),
        )]));
        let result: Result<Vec<CompletionDelta>, ProviderError> =
            futures::executor::block_on(async move {
                let mut v = Vec::new();
                futures::pin_mut!(s);
                while let Some(item) = s.next().await {
                    v.push(item?);
                }
                Ok(v)
            });
        // Smoke: does not panic; returns Ok(empty) or Err cleanly.
        let _ = result;
    }
}
