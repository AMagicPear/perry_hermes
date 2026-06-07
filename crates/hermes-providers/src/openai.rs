//! `OpenAiProvider` — real OpenAI Chat Completions adapter.
//!
//! Phase 2 minimum: POST `{base_url}/chat/completions` with the
//! serialized request, parse the response, and map the `finish_reason`
//! string to our `FinishReason` enum. Tool-call parsing, streaming,
//! retries, and richer error mapping land in later phases.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use hermes_core::message::{Content, ContentPart, Message};
use hermes_core::provider::{CompletionDelta, FinishReason, Provider};
use hermes_core::registry::ToolSchema;
use hermes_core::{CompletionStream, ProviderError};

pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Override the API base URL. Tests point this at a local mock
    /// server; users can use it to talk to Azure / Together / a proxy.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OaiTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    stream: bool,
    /// Ask OpenAI to include token usage in the stream's final chunk
    /// (otherwise `in`/`out` metrics stay at 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct OaiMessage<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<OaiMessageContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    /// Round-trip the LLM's `tool_calls` so the next request
    /// remembers which tools it invoked. OpenAI expects each entry as
    /// `{ id, type: "function", function: { name, arguments } }`.
    /// `arguments` is sent as a JSON *string* (matching how OpenAI
    /// returns it), not a nested object.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallRef<'a>>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum OaiMessageContent<'a> {
    Text(&'a str),
    Parts(Vec<OaiContentPart<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OaiContentPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: OaiImageUrl<'a> },
}

#[derive(Serialize)]
struct OaiImageUrl<'a> {
    url: &'a str,
}

#[derive(Serialize)]
struct OaiToolCallRef<'a> {
    id: &'a str,
    r#type: &'static str, // "function"
    function: OaiFunctionCallRef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionCallRef<'a> {
    name: &'a str,
    arguments: String,
}

#[derive(Serialize)]
struct OaiTool<'a> {
    r#type: &'static str,
    function: OaiFunctionDef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionDef<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

fn build_request_body<'a>(
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolSchema],
    stream: bool,
) -> Result<ChatRequest<'a>, ProviderError> {
    let oai_msgs: Vec<OaiMessage> = messages
        .iter()
        .map(|m| {
            let tool_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| {
                        let arguments =
                            serde_json::to_string(&c.arguments).unwrap_or_else(|_| "null".into());
                        OaiToolCallRef {
                            id: &c.id,
                            r#type: "function",
                            function: OaiFunctionCallRef {
                                name: &c.name,
                                arguments,
                            },
                        }
                    })
                    .collect()
            });
            OaiMessage {
                role: m.role.as_str(),
                content: match &m.content {
                    Content::Text(s) => Some(OaiMessageContent::Text(s.as_str())),
                    Content::Parts(parts) => Some(OaiMessageContent::Parts(
                        parts
                            .iter()
                            .map(|p| match p {
                                ContentPart::Text { text } => OaiContentPart::Text {
                                    text: text.as_str(),
                                },
                                ContentPart::ImageUrl { url } => OaiContentPart::ImageUrl {
                                    image_url: OaiImageUrl { url: url.as_str() },
                                },
                            })
                            .collect(),
                    )),
                },
                tool_call_id: m.tool_call_id.as_deref(),
                tool_calls,
            }
        })
        .collect();

    let oai_tools: Vec<OaiTool> = tools
        .iter()
        .map(|t| OaiTool {
            r#type: "function",
            function: OaiFunctionDef {
                name: &t.name,
                description: &t.description,
                parameters: &t.parameters,
            },
        })
        .collect();

    let has_tools = !oai_tools.is_empty();
    Ok(ChatRequest {
        model,
        messages: oai_msgs,
        tools: oai_tools,
        tool_choice: if has_tools { Some("auto") } else { None },
        stream,
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
    })
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let body = build_request_body(&self.model, messages, tools, true)?;
        let url = format!("{}/chat/completions", self.base_url);

        // Pre-flight: send request, race against cancel
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self.client.post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send() => r.map_err(|e| ProviderError::Transport(e.to_string()))?,
        };

        // Pre-flight: status check
        if resp.status() == 401 {
            return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
        }
        if resp.status() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_secs: 1,
            });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        // Hand the byte stream to the SSE parser.
        Ok(Box::pin(parse_sse_chunks(resp.bytes_stream())))
    }
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn parse_sse_for_test(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
    use futures::stream;
    // stream::iter on a Vec yields a Unpin stream — needed for Box::pin inside
    // parse_sse_chunks.
    let stream = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(input))]);
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

fn parse_sse_chunks(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Unpin,
) -> impl Stream<Item = Result<CompletionDelta, ProviderError>> {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut state = OpenAiStreamState::default();
        let mut bytes = Box::pin(bytes);
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(c) => buffer.push_str(&String::from_utf8_lossy(&c)),
                Err(e) => { yield Err(ProviderError::Transport(e.to_string())); return; }
            }
            while let Some(pos) = buffer.find("\n\n") {
                let event: String = buffer.drain(..pos + 2).collect();
                for line in event.lines() {
                    let Some(rest) = line.strip_prefix("data: ") else { continue };
                    let payload = rest.trim();
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

#[derive(Default)]
struct OpenAiStreamState {
    in_think_tag: bool,
}

fn parse_sse_data_payload(
    payload: &str,
    state: &mut OpenAiStreamState,
) -> Result<CompletionDelta, ProviderError> {
    #[derive(serde::Deserialize)]
    struct SseChunk {
        choices: Vec<SseChoice>,
        #[serde(default)]
        usage: Option<hermes_core::Usage>,
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
    // `usage` when `stream_options.include_usage=true`. Handle by returning
    // a delta carrying only the usage.
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
        calls
            .into_iter()
            .next()
            .map(|c| hermes_core::provider::ToolCallDelta {
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

fn split_think_tag_content(
    content: Option<String>,
    state: &mut OpenAiStreamState,
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

    (
        if visible.is_empty() {
            None
        } else {
            Some(visible)
        },
        if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sse_bytes(input: &[u8]) -> Result<Vec<CompletionDelta>, ProviderError> {
        parse_sse_for_test(input)
    }

    #[test]
    fn parses_single_text_chunk() {
        let sse =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let deltas = parse_sse_bytes(sse).unwrap();
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].content_delta.as_deref(), Some("Hel"));
        assert_eq!(deltas[1].content_delta.as_deref(), Some("lo"));
        assert_eq!(deltas[2].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn done_marker_terminates() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let result = parse_sse_bytes(sse);
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }

    #[test]
    fn comment_lines_are_skipped() {
        let sse = b": this is a comment\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let deltas = parse_sse_bytes(sse).unwrap();

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
        let deltas = parse_sse_bytes(sse).unwrap();
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
        let result = parse_sse_bytes(sse);
        // Smoke: does not panic; returns either Ok or Err cleanly.
        let _ = result;
    }
}
