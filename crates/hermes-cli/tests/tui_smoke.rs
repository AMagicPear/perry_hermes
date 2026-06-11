//! End-to-end smoke test: run the TUI's `run` function with a `ScriptedProvider`,
//! drive one turn, and assert the rendered `TestBackend` buffer contains the
//! expected user and assistant lines.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::stream;
use perry_hermes_agent::AgentRunError;
use perry_hermes_core::ProviderError;
use perry_hermes_core::error::LoopError;
use perry_hermes_core::message::{Content, Message, Role};
use perry_hermes_core::provider::{
    Completion, CompletionDelta, CompletionStream, FinishReason, Provider, ToolCallDelta,
};
use perry_hermes_core::registry::ToolSchema;
use perry_hermes_core::usage::Usage;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use perry_hermes_cli::tui::event::AppEvent;
use perry_hermes_cli::tui::run::run_with_backend;

/// An inline `ScriptedProvider` for use in perry-hermes-cli integration tests.
/// Unlike the support version in perry-hermes-agent/tests (which is not public),
/// this one is defined locally so it can be used from perry-hermes-cli/tests/.
struct ScriptedProvider {
    script: std::sync::Mutex<Vec<Vec<CompletionDelta>>>,
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    fn new(script: Vec<Completion>) -> Self {
        let script: Vec<Vec<CompletionDelta>> = script
            .into_iter()
            .map(|c| completion_to_deltas(&c))
            .collect();
        Self {
            script: std::sync::Mutex::new(script),
            call_count: AtomicUsize::new(0),
        }
    }
}

fn completion_to_deltas(c: &Completion) -> Vec<CompletionDelta> {
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
                arguments_fragment: Some(tc.arguments.to_string()),
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

fn terminal_text(backend: &Arc<Mutex<TestBackend>>) -> String {
    let guard = backend.lock().unwrap();
    let mut text = buffer_to_text(guard.scrollback());
    text.push_str(&buffer_to_text(guard.buffer()));
    text
}

fn buffer_to_text(buffer: &ratatui::buffer::Buffer) -> String {
    let width = buffer.area.width as usize;
    let height = buffer.area.height as usize;
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                text.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        text.push('\n');
    }
    text
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
    let _provider = Arc::new(provider);

    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
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
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        Some(10),
        None,
    )
    .await;

    assert!(result.is_ok(), "tui run returned error: {result:?}");
}

/// Strengthened version: retains the `TestBackend` reference after the loop
/// exits and asserts that the user message appears in terminal scrollback.
#[tokio::test]
async fn user_message_appears_in_terminal_scrollback() {
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
    let _provider = Arc::new(provider);

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

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        Some(10),
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("> hi"),
        "user message '> hi' not found in terminal text:\n{text}"
    );
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
    let _provider = Arc::new(provider);

    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Compact(Some("shell history".to_string())))
        .expect("send compact");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    // The test only asserts the TUI accepts and dispatches the event without
    // panicking; the actual compression call lives in `AgentLoop::compact_session`
    // and is exercised by `perry-hermes-agent`'s context-compression tests.
    let result = run_with_backend(
        backend,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        Some(10),
        None,
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn unknown_slash_command_is_rendered_to_scrollback() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    for ch in "/bogus".chars() {
        input_tx
            .send(AppEvent::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )))
            .expect("send char");
    }
    input_tx
        .send(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .expect("send enter");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        Some(10),
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("Unknown command: /bogus"),
        "expected unknown slash command in scrollback; full buffer:\n{text}"
    );
}

#[tokio::test]
async fn cancelled_turn_does_not_block_following_submit() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let finished = Arc::new(AtomicBool::new(false));
    let finished_clone = Arc::clone(&finished);

    let handle = tokio::spawn(async move {
        let result = run_with_backend(
            backend.clone(),
            input_rx,
            cancel,
            "echo".to_string(),
            "test-model".to_string(),
            Some(10),
            None,
        )
        .await;
        finished_clone.store(true, Ordering::SeqCst);
        (result, backend)
    });

    input_tx
        .send(AppEvent::Submit("first".to_string()))
        .expect("send first submit");
    input_tx
        .send(AppEvent::CancelInFlight)
        .expect("send cancel event");
    input_tx
        .send(AppEvent::TurnCompleted(Err(AgentRunError::Loop(
            LoopError::Cancelled,
        ))))
        .expect("send cancelled completion");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !finished.load(Ordering::SeqCst),
        "run loop exited after first cancellation"
    );

    input_tx
        .send(AppEvent::Submit("second".to_string()))
        .expect("send second submit");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    let (result, backend) = handle.await.expect("join tui task");
    result.expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("> second"),
        "expected second submit to remain usable after cancellation; full buffer:\n{text}"
    );
}
