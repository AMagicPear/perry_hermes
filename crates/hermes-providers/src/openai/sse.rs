//! SSE parser for the OpenAI Chat Completions stream.
//!
//! Splits the byte stream on `\n\n` event boundaries, parses each
//! `data: ...` line as JSON, and yields `CompletionDelta`s.
//!
//! `split_think_tag_content` reclassifies content that arrives between
//! `<think>...</think>` tags as reasoning. This is non-standard for
//! OpenAI (only some reasoning models emit it) but matches the
//! `OpenAiStreamState` invariant used in the tests.

use bytes::Bytes;
use futures::{Stream, StreamExt};

use perry_hermes_core::ProviderError;
use perry_hermes_core::provider::{CompletionDelta, FinishReason, ToolCallDelta};

use crate::http::transport_error_message;

/// Strip the `data: ` prefix and surrounding whitespace; return the
/// payload, or `None` if the line is a comment or a control line.
pub(super) fn parse_sse_data_line(line: &str) -> Option<&str> {
    line.strip_prefix("data: ").map(str::trim)
}

/// Per-stream state carried across delta yields.
#[derive(Default)]
pub(super) struct OpenaiStreamState {
    /// `true` after a `<think>` tag has been seen but before the
    /// matching `</think>` arrives; used by `split_think_tag_content`.
    pub in_think_tag: bool,
}

pub(super) fn parse_sse_chunks(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut state = OpenaiStreamState::default();
        let mut bytes = Box::pin(bytes);
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => buffer.push_str(&String::from_utf8_lossy(&c)),
                Err(e) => { yield Err(ProviderError::Transport(transport_error_message(&e))); return; }
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                for line in event.lines() {
                    let Some(payload) = parse_sse_data_line(line) else { continue };
                    if payload == "[DONE]" { return; }
                    match parse_sse_data_payload(payload, &mut state) {
                        Ok(d) => yield Ok(d),
                        Err(e) => { yield Err(e); return; }
                    }
                }
            }
        }
    }
}

fn parse_sse_data_payload(
    payload: &str,
    state: &mut OpenaiStreamState,
) -> Result<CompletionDelta, ProviderError> {
    #[derive(serde::Deserialize)]
    struct SseChunk {
        choices: Vec<SseChoice>,
        #[serde(default)]
        usage: Option<perry_hermes_core::Usage>,
    }
    #[derive(serde::Deserialize)]
    struct SseChoice {
        delta: SseDelta,
        finish_reason: Option<String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct SseDelta {
        content: Option<String>,
        #[serde(default)]
        reasoning_content: Option<String>,
        tool_calls: Option<Vec<SseToolCallRef>>,
    }
    #[derive(serde::Deserialize)]
    struct SseToolCallRef {
        index: usize,
        id: Option<String>,
        function: SseFunction,
    }
    #[derive(serde::Deserialize, Default)]
    struct SseFunction {
        name: Option<String>,
        arguments: Option<String>,
    }

    let chunk: SseChunk = serde_json::from_str(payload)
        .map_err(|e| ProviderError::InvalidResponse(format!("sse json: {e}")))?;

    // OpenAI sends a final chunk with empty `choices` and a populated
    // `usage` when `stream_options.include_usage=true`. Surface just the
    // usage; the consumer's accumulator merges it into the next delta.
    let Some(choice) = chunk.choices.into_iter().next() else {
        return Ok(CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: None,
            usage: chunk.usage,
            finish_reason: None,
        });
    };

    let tool_call_delta = choice.delta.tool_calls.and_then(|calls| {
        calls.into_iter().next().map(|c| ToolCallDelta {
            index: c.index,
            id: c.id,
            name: c.function.name,
            arguments_fragment: c.function.arguments,
        })
    });

    let (content_delta, reasoning_delta) = match choice.delta.reasoning_content {
        Some(reasoning) => (choice.delta.content, Some(reasoning)),
        None => split_think_tag_content(choice.delta.content, state),
    };

    Ok(CompletionDelta {
        content_delta,
        reasoning_delta,
        tool_call_delta,
        usage: chunk.usage,
        finish_reason: choice
            .finish_reason
            .as_deref()
            .map(FinishReason::from_provider_str),
    })
}

/// Split a content chunk along `<think>...</think>` boundaries.
/// Content inside the tags is reported as `reasoning`; everything else
/// stays in `content`. The state flag persists across calls so tags can
/// open and close across chunk boundaries.
fn split_think_tag_content(
    content: Option<String>,
    state: &mut OpenaiStreamState,
) -> (Option<String>, Option<String>) {
    let Some(content) = content else {
        return (None, None);
    };

    let mut rest = content.as_str();
    let mut visible = String::new();
    let mut reasoning = String::new();

    while !rest.is_empty() {
        if state.in_think_tag {
            if let Some(end) = rest.find("</think>") {
                reasoning.push_str(&rest[..end]);
                rest = &rest[end + "</think>".len()..];
                state.in_think_tag = false;
            } else {
                reasoning.push_str(rest);
                rest = "";
            }
        } else if let Some(start) = rest.find("<think>") {
            visible.push_str(&rest[..start]);
            rest = &rest[start + "<think>".len()..];
            state.in_think_tag = true;
        } else {
            visible.push_str(rest);
            rest = "";
        }
    }

    let content = if visible.is_empty() {
        None
    } else {
        Some(visible)
    };
    let reasoning = if reasoning.is_empty() {
        None
    } else {
        Some(reasoning)
    };
    (content, reasoning)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the parser over a single byte chunk and collect every delta.
    /// `stream::iter` on a `Vec` yields a `Unpin` stream, which is what
    /// `parse_sse_chunks` expects inside `Box::pin`.
    fn parse_sse_chunk(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
        let stream =
            futures::stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(input))]);
        let s = parse_sse_chunks(stream);
        futures::executor::block_on(async move {
            let mut v = Vec::new();
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                v.push(item?);
            }
            Ok(v)
        })
    }

    #[test]
    fn parses_single_text_chunk() {
        let sse =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hello"));
    }

    #[test]
    fn parses_multiple_text_chunks() {
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn done_marker_terminates() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("x"));
    }

    #[test]
    fn tool_call_chunks_assemble() {
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 4);
        let first_tool = deltas[0].tool_call_delta.as_ref().unwrap();
        assert_eq!(first_tool.index, 0);
        assert_eq!(first_tool.id.as_deref(), Some("call_a"));
        assert_eq!(first_tool.name.as_deref(), Some("bash"));
        assert_eq!(deltas[3].finish_reason, Some(FinishReason::ToolUse));
    }

    #[test]
    fn malformed_json_yields_error() {
        let sse = b"data: {not valid json}\n\n";
        let result = parse_sse_chunk(sse);
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }

    #[test]
    fn comment_lines_are_skipped() {
        let sse = b": this is a comment\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 1);
    }

    #[test]
    fn usage_only_chunk_with_empty_choices_is_parsed() {
        // OpenAI sends a final chunk with empty choices + usage when
        // stream_options.include_usage=true is set. The parser must NOT
        // error on this — it should return a delta with just usage set.
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"total_tokens\":16,\"cached_tokens\":0}}\n\n\
data: [DONE]\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("hi"));
        let usage = deltas[1]
            .usage
            .expect("usage-only chunk must surface usage");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 4);
        // The usage-only chunk has no choice, so all choice-derived fields are None.
        assert!(deltas[1].content_delta.is_none());
        assert!(deltas[1].reasoning_delta.is_none());
        assert!(deltas[1].tool_call_delta.is_none());
        assert!(deltas[1].finish_reason.is_none());
    }

    #[test]
    fn think_tag_content_is_reclassified_as_reasoning() {
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"<think>Need\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" to inspect</think>Answer\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" done\"},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();

        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].reasoning_delta.as_deref(), Some("Need"));
        assert!(deltas[0].content_delta.is_none());
        assert_eq!(deltas[1].reasoning_delta.as_deref(), Some(" to inspect"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("Answer"));
        assert_eq!(deltas[2].content_delta.as_deref(), Some(" done"));
        assert!(deltas[2].reasoning_delta.is_none());
    }

    #[test]
    fn chunks_split_across_frames_assemble_correctly() {
        // A single SSE event must survive being split across multiple
        // byte chunks. The "\n\n" frame boundary can land in the middle
        // of a chunk; the parser must still produce one delta.
        use futures::stream;
        let chunks: Vec<&[u8]> = vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n",
            b"\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
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
    fn done_sentinel_preserves_prior_deltas() {
        // Two valid events, then DONE, then a third valid event. The
        // third must NOT be parsed — the stream terminates at the
        // sentinel. (Note: the existing `done_marker_terminates` test
        // covers the 1-event case; this one specifically asserts that
        // both prior deltas are kept.)
        let sse = b"\
data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n\
data: [DONE]\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"z\"}}]}\n\n";
        let deltas = parse_sse_chunk(sse).unwrap();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("x"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("y"));
    }

    #[test]
    fn partial_utf8_in_a_chunk_does_not_panic() {
        // Bytes containing invalid UTF-8 inside a data: line. The
        // parser uses String::from_utf8_lossy, so it must not panic.
        // The exact outcome is unspecified (could be Err
        // InvalidResponse for the malformed JSON, or skip the event);
        // we only assert that the call returns rather than panicking.
        let sse = b"data: \xFF\xFE\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n";
        let result = parse_sse_chunk(sse);
        // Smoke: does not panic; returns either Ok or Err cleanly.
        let _ = result;
    }
}
