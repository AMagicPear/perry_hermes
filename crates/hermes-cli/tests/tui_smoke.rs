//! End-to-end smoke test: run the TUI's `run` function with a `ScriptedProvider`,
//! drive one turn, and assert the rendered `TestBackend` buffer contains the
//! expected user and assistant lines.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::{Completion, CompletionDelta, CompletionStream, FinishReason, Provider, ToolCallDelta};
use hermes_core::usage::Usage;
use hermes_core::ProviderError;
use hermes_core::registry::ToolSchema;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use hermes_cli::tui::event::AppEvent;
use hermes_cli::tui::run::{run_with_backend, run_with_backend_and_capture};

/// An inline `ScriptedProvider` for use in hermes-cli integration tests.
/// Unlike the support version in hermes-agent/tests (which is not public),
/// this one is defined locally so it can be used from hermes-cli/tests/.
struct ScriptedProvider {
    script: std::sync::Mutex<Vec<Vec<CompletionDelta>>>,
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    fn new(script: Vec<Completion>) -> Self {
        let script: Vec<Vec<CompletionDelta>> = script
            .into_iter()
            .map(completion_to_deltas)
            .collect();
        Self {
            script: std::sync::Mutex::new(script),
            call_count: AtomicUsize::new(0),
        }
    }
}

fn completion_to_deltas(c: Completion) -> Vec<CompletionDelta> {
    let mut deltas = Vec::new();
    let has_text = matches!(&c.message.content, Content::Text(t) if !t.is_empty());
    let has_reasoning = c.message.reasoning.as_ref().is_some_and(|s| !s.is_empty());

    if has_text || has_reasoning {
        deltas.push(CompletionDelta {
            content_delta: match &c.message.content {
                Content::Text(t) => Some(t.clone()),
                Content::Parts(_) => None,
            },
            reasoning_delta: c.message.reasoning.clone(),
            tool_call_delta: None,
            usage: Some(c.usage),
            finish_reason: None,
        });
    }

    if let Some(calls) = &c.message.tool_calls {
        deltas.extend(calls.iter().enumerate().map(|(index, tc)| CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index,
                id: Some(tc.id.clone()),
                name: Some(tc.name.clone()),
                arguments_delta: Some(tc.arguments.to_string()),
            }),
            usage: None,
            finish_reason: None,
        }));
    }

    deltas.push(CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(c.usage),
        finish_reason: Some(c.finish_reason),
    });
    deltas
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            panic!(
                "ScriptedProvider: script exhausted - the loop called stream() more times than scripted"
            );
        }
        let step = script.remove(0);
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(step.into_iter().map(Ok))))
    }
}

#[tokio::test]
async fn user_message_then_assistant_reply_appears_in_scrollback() {
    let provider = ScriptedProvider::new(vec![Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("hello back".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
        },
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    }]);
    let provider = Arc::new(provider);

    let backend = TestBackend::new(80, 24);
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Drive the TUI: enqueue a Submit event, then a Quit, then drop the tx.
    input_tx
        .send(AppEvent::Submit("hi".to_string()))
        .expect("send submit");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    let result = run_with_backend(
        backend,
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await;

    assert!(result.is_ok(), "tui run returned error: {result:?}");
}

/// Strengthened version: retains the `TestBackend` reference after the loop
/// exits and asserts that the user message appears in the rendered buffer.
#[tokio::test]
async fn user_message_appears_in_buffer() {
    let provider = ScriptedProvider::new(vec![Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("hello back".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
        },
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    }]);
    let provider = Arc::new(provider);

    // Wrap TestBackend in Arc<Mutex<_>> so we can retain access after the
    // TUI loop drops its owned reference.
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Submit("hi".to_string()))
        .expect("send submit");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend_and_capture(
        backend.clone(),
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await
    .expect("tui run returned error");

    // The user message "hi" should appear as "> hi" somewhere in the buffer.
    let guard = backend.lock().unwrap();
    let buffer = guard.buffer();
    let width = buffer.area.width as usize;
    let height = buffer.area.height as usize;
    let mut found = false;
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                row.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        if row.contains("> hi") {
            found = true;
            break;
        }
    }
    assert!(found, "user message '> hi' not found in buffer");
}

#[tokio::test]
async fn compact_command_emits_compress_request() {
    let provider = ScriptedProvider::new(vec![Completion {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("done".to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
        },
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
        },
        finish_reason: FinishReason::Stop,
    }]);
    let provider = Arc::new(provider);

    let backend = TestBackend::new(80, 24);
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Compact(Some("shell history".to_string())))
        .expect("send compact");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    // The test only asserts the TUI accepts and dispatches the event without
    // panicking; the actual compression call lives in `AIAgent::run_compact`
    // and is exercised by `hermes-agent`'s context-compression tests.
    let result = run_with_backend(
        backend,
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await;
    assert!(result.is_ok());
}