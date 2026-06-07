//! The TUI's main entry point. A test-friendly `run_with_backend` variant
//! accepts a `TestBackend` and an injected input channel; the production
//! `run` function wraps it with `CrosstermBackend::Stdout`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use perry_hermes_agent::{AIAgent, AgentRunError, AgentSession, SessionContext};
use perry_hermes_core::error::LoopError;
use perry_hermes_core::tool::ToolOutput;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Cell;
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crate::tui::history::{render_history_lines_to_buffer, HistoryWrite};
use crate::tui::input::handle_key;
use crate::tui::loop_bridge::apply_loop_event;
use crate::tui::make_on_event;
use crate::tui::render::render_bottom;

const INLINE_VIEWPORT_HEIGHT: u16 = 6;

/// Local error type for the TUI run loop.
#[derive(Debug)]
pub enum RunError {
    Tui(String),
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Tui(s) => write!(f, "Tui({s})"),
            RunError::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<AgentRunError> for RunError {
    fn from(e: AgentRunError) -> Self {
        RunError::Tui(e.to_string())
    }
}

/// Production entry point: drives the TUI against stdout / real keyboard.
pub async fn run(
    agent: Arc<AIAgent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
    context_window_size: Option<u64>,
) -> Result<(), RunError> {
    use crossterm::event::{Event, EventStream};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::stdout;

    enable_raw_mode().map_err(|e| RunError::Tui(e.to_string()))?;
    let backend = WideCellSafeBackend::new(CrosstermBackend::new(stdout()));
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(INLINE_VIEWPORT_HEIGHT),
        },
    )
    .map_err(|e| RunError::Tui(e.to_string()))?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;
    let mut history = HistoryWrite::default();

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(16));

    let session = AgentSession::new(SessionContext {
        working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        session_id: "cli".into(),
    });

    let result: Result<(), RunError> = async {
        loop {
            draw_inline_bottom(&mut terminal, &mut app, &mut history)?;

            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let width = app.history_width;
                    history.push(&mut app, RenderedLine::System("⚠ cancelled".to_string()), width);
                    draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                    return Ok(());
                }
                _ = tick.tick() => {
                    // Periodic redraw keeps the display fresh while streaming.
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(k))) => {
                            let next = handle_key(&mut app, k);
                            if dispatch_event(
                                &mut app,
                                next,
                                &cancel,
                                Some(RunContext {
                                    agent: &agent,
                                    session: &session,
                                    input_tx: &input_tx,
                                }),
                                Some(&mut history),
                            )? {
                                draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                                return Ok(());
                            }
                        }
                        Some(Ok(Event::Resize(_, _))) => {
                            // Next tick will redraw at the new size.
                        }
                        Some(Err(e)) => {
                            return Err(RunError::Tui(e.to_string()));
                        }
                        None => return Ok(()),
                        _ => {}
                    }
                }
                maybe = input_rx.recv() => {
                    if let Some(ev) = maybe {
                        if dispatch_event(&mut app, ev, &cancel, None, Some(&mut history))? {
                            draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    .await;

    if let Err(e) = disable_raw_mode() {
        eprintln!("[perry-hermes] warning: failed to disable raw mode: {e}");
    }
    result
}

/// Test-friendly entry point. The caller supplies:
/// - an `Arc<Mutex<TestBackend>>` (the test can retain a clone to inspect
///   the buffer after the loop returns)
/// - a stream of `AppEvent`s (the test enqueues Submit + Quit)
/// - a `CancellationToken`
/// - the provider/model name for the status bar
///
/// Returns when the input channel is closed and the main loop observes no
/// more events.
pub async fn run_with_backend(
    backend: Arc<Mutex<ratatui::backend::TestBackend>>,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
    context_window_size: Option<u64>,
) -> Result<(), RunError> {
    let backend = WideCellSafeBackend::new(SharedTestBackend { inner: backend });
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(INLINE_VIEWPORT_HEIGHT),
        },
    )
    .map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;
    let mut history = HistoryWrite::default();

    loop {
        draw_inline_bottom(&mut terminal, &mut app, &mut history)?;

        tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                let width = app.history_width;
                history.push(&mut app, RenderedLine::System("⚠ cancelled".to_string()), width);
                draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                return Ok(());
            }
            maybe = input_rx.recv() => {
                let Some(ev) = maybe else {
                    draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                    return Ok(());
                };
                if dispatch_event(&mut app, ev, &cancel, None, Some(&mut history))? {
                    draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                    return Ok(());
                }
            }
        }
    }
}

struct RunContext<'a> {
    agent: &'a Arc<AIAgent>,
    session: &'a AgentSession,
    input_tx: &'a mpsc::UnboundedSender<AppEvent>,
}

fn dispatch_event(
    app: &mut App,
    ev: AppEvent,
    cancel: &CancellationToken,
    run_ctx: Option<RunContext<'_>>,
    mut history: Option<&mut HistoryWrite>,
) -> Result<bool, RunError> {
    match ev {
        AppEvent::Key(k) => {
            let next = handle_key(app, k);
            dispatch_event(app, next, cancel, run_ctx, history)
        }
        AppEvent::Loop(loop_ev) => {
            if let Some(history) = history.as_mut() {
                match &loop_ev {
                    perry_hermes_agent::LoopEvent::ContentDelta(text) => {
                        history.push_assistant_delta(text, app.history_width);
                        return Ok(false);
                    }
                    perry_hermes_agent::LoopEvent::ReasoningDelta(text) => {
                        history.push_reasoning_delta(text, app.history_width);
                        return Ok(false);
                    }
                    perry_hermes_agent::LoopEvent::AssistantMessage(_) => {
                        history.finish_stream(app.history_width);
                    }
                    perry_hermes_agent::LoopEvent::ToolCallStarted { call, .. } => {
                        history.finish_stream(app.history_width);
                        let line = RenderedLine::ToolCall {
                            name: call.name.clone(),
                            args_preview: call.arguments.to_string(),
                        };
                        history.push(app, line, app.history_width);
                        return Ok(false);
                    }
                    perry_hermes_agent::LoopEvent::ToolCallFinished { call, result } => {
                        history.finish_stream(app.history_width);
                        let line = RenderedLine::ToolResult {
                            name: call.name.clone(),
                            output: match result {
                                Ok(output) => summarize_tool_output_for_history(&call.name, output),
                                Err(e) => e.to_string(),
                            },
                            ok: result.is_ok(),
                        };
                        history.push(app, line, app.history_width);
                        return Ok(false);
                    }
                    _ => {}
                }
            }
            let next = apply_loop_event(app, loop_ev);
            let _ = dispatch_event(app, next, cancel, run_ctx, history)?;
            Ok(false)
        }
        AppEvent::Tick => Ok(false),
        AppEvent::Submit(text) => {
            push_history_or_scrollback(
                app,
                &mut history,
                RenderedLine::User(text.clone()),
                app.history_width,
            );
            app.mode = AppMode::AwaitingModel;
            app.turn_started_at = Some(Instant::now());
            if app.active_turn_cancel.is_none() {
                app.active_turn_cancel = Some(CancellationToken::new());
            }
            if let Some(ctx) = run_ctx {
                let turn_cancel = CancellationToken::new();
                let on_event = make_on_event(ctx.input_tx.clone());
                let agent = Arc::clone(ctx.agent);
                let session = ctx.session.clone();
                let result_tx = ctx.input_tx.clone();
                app.active_turn_cancel = Some(turn_cancel.clone());
                tokio::spawn(async move {
                    let res = agent
                        .run_session_turn(&text, &session, turn_cancel, on_event)
                        .await;
                    let _ = result_tx.send(AppEvent::TurnCompleted(res));
                });
            }
            Ok(false)
        }
        AppEvent::Quit => Ok(true),
        AppEvent::Compact(focus) => {
            let line = RenderedLine::System(format!(
                "Manual compact requested (focus: {}).",
                focus.as_deref().unwrap_or("(none)")
            ));
            let width = app.history_width;
            push_history_or_scrollback(app, &mut history, line, width);

            if let Some(ctx) = run_ctx {
                app.mode = AppMode::AwaitingModel;
                app.turn_started_at = Some(Instant::now());
                let agent = Arc::clone(ctx.agent);
                let session = ctx.session.clone();
                let result_tx = ctx.input_tx.clone();
                tokio::spawn(async move {
                    let res = agent.compact_session(&session, focus.as_deref()).await;
                    let _ = result_tx.send(AppEvent::CompactCompleted(res));
                });
            } else {
                app.compression_hint = Some("No agent attached for compact".to_string());
            }
            Ok(false)
        }
        AppEvent::Clear => {
            app.clear_scrollback();
            if let Some(history) = history.as_mut() {
                history.clear();
            }
            if let Some(ctx) = run_ctx {
                let session = ctx.session.clone();
                tokio::spawn(async move {
                    session.reset().await;
                });
            }
            Ok(false)
        }
        AppEvent::Append(line) => {
            let width = app.history_width;
            push_history_or_scrollback(app, &mut history, line, width);
            Ok(false)
        }
        AppEvent::SetInput(s) => {
            let len = s.len();
            app.input = s;
            app.cursor = len;
            Ok(false)
        }
        AppEvent::CancelInFlight => {
            app.mode = AppMode::Cancelling;
            if let Some(turn_cancel) = app.active_turn_cancel.take() {
                turn_cancel.cancel();
            } else {
                cancel.cancel();
            }
            Ok(false)
        }
        AppEvent::TurnCompleted(res) => {
            app.turn_started_at = None;
            app.active_turn_cancel = None;
            match res {
                Ok(_) => {}
                Err(AgentRunError::Loop(LoopError::CancelledWith(_))) => {
                    if let Some(history) = history.as_mut() {
                        history.finish_stream(app.history_width);
                    }
                }
                Err(AgentRunError::Loop(LoopError::Cancelled)) => {
                    if let Some(history) = history.as_mut() {
                        history.finish_stream(app.history_width);
                    }
                }
                Err(e) => {
                    if let Some(history) = history.as_mut() {
                        history.finish_stream(app.history_width);
                    }
                    let line = RenderedLine::System(format!("error: {e}"));
                    let width = app.history_width;
                    push_history_or_scrollback(app, &mut history, line, width);
                }
            }
            app.mode = AppMode::Idle;
            Ok(false)
        }
        AppEvent::CompactCompleted(res) => {
            app.turn_started_at = None;
            app.active_turn_cancel = None;
            match res {
                Ok(event) => {
                    let next = apply_loop_event(app, event);
                    let _ = dispatch_event(app, next, cancel, None, history)?;
                }
                Err(AgentRunError::Loop(LoopError::CancelledWith(_))) => {}
                Err(AgentRunError::Loop(LoopError::Cancelled)) => {}
                Err(e) => {
                    let line = RenderedLine::System(format!("error: {e}"));
                    let width = app.history_width;
                    push_history_or_scrollback(app, &mut history, line, width);
                }
            }
            app.mode = AppMode::Idle;
            Ok(false)
        }
    }
}

fn summarize_tool_output_for_history(tool_name: &str, output: &ToolOutput) -> String {
    if tool_name == "read_file" {
        return summarize_read_file_output_for_history(&output.content);
    }
    summarize_generic_tool_output_for_history(&output.content)
}

fn summarize_read_file_output_for_history(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return summarize_generic_tool_output_for_history(raw);
    };
    let Some(content) = value.get("content").and_then(|v| v.as_str()) else {
        return summarize_generic_tool_output_for_history(raw);
    };
    summarize_generic_tool_output_for_history(content)
}

fn summarize_generic_tool_output_for_history(raw: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 20;
    const MAX_PREVIEW_CHARS: usize = 4_000;

    let mut preview = raw
        .lines()
        .take(MAX_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        preview = preview.chars().take(MAX_PREVIEW_CHARS).collect();
        preview.push('…');
        return preview;
    }
    if raw.lines().count() > MAX_PREVIEW_LINES {
        preview.push_str("\n…");
    }
    preview
}

fn push_history_or_scrollback(
    app: &mut App,
    history: &mut Option<&mut HistoryWrite>,
    line: RenderedLine,
    width: u16,
) {
    if let Some(history) = history.as_mut() {
        history.push(app, line, width);
    } else {
        app.push_line(line);
    }
}

fn draw_inline_bottom<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    history: &mut HistoryWrite,
) -> Result<(), RunError>
where
    B: Backend,
{
    let size = terminal.size().map_err(|e| RunError::Tui(e.to_string()))?;
    app.history_width = size.width;

    let pending = history.drain();
    if !pending.is_empty() {
        let height = pending.len().min(u16::MAX as usize) as u16;
        terminal
            .insert_before(height, |buffer| {
                render_history_lines_to_buffer(&pending, buffer)
            })
            .map_err(|e| RunError::Tui(e.to_string()))?;
    }

    terminal
        .draw(|f| render_bottom(f, app))
        .map_err(|e| RunError::Tui(e.to_string()))?;
    Ok(())
}

struct WideCellSafeBackend<B> {
    inner: B,
}

impl<B> WideCellSafeBackend<B> {
    fn new(inner: B) -> Self {
        Self { inner }
    }
}

impl<B: Backend> Backend for WideCellSafeBackend<B> {
    fn draw<'a, I>(&mut self, content: I) -> Result<(), std::io::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut filtered = Vec::new();
        let mut last_y: Option<u16> = None;
        let mut skip_until_x = 0u16;

        for (x, y, cell) in content {
            if last_y != Some(y) {
                skip_until_x = 0;
                last_y = Some(y);
            }

            if x >= skip_until_x {
                filtered.push((x, y, cell));
            }

            let width = cell.symbol().width().max(1).min(u16::MAX as usize) as u16;
            skip_until_x = x.saturating_add(width);
        }

        self.inner.draw(filtered.into_iter())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), std::io::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), std::io::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), std::io::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, std::io::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        pos: P,
    ) -> Result<(), std::io::Error> {
        self.inner.set_cursor_position(pos)
    }

    fn clear(&mut self) -> Result<(), std::io::Error> {
        self.inner.clear()
    }

    fn clear_region(
        &mut self,
        clear_type: ratatui::backend::ClearType,
    ) -> Result<(), std::io::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<ratatui::layout::Size, std::io::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, std::io::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.inner.flush()
    }
}

/// A `Backend` wrapper around `Arc<Mutex<TestBackend>>` so that:
/// - the TUI's `Terminal` can borrow the backend during the loop
/// - the caller retains an `Arc` clone to inspect the buffer afterward
struct SharedTestBackend {
    inner: Arc<Mutex<ratatui::backend::TestBackend>>,
}

impl Backend for SharedTestBackend {
    fn draw<'a, I>(&mut self, content: I) -> Result<(), std::io::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        let mut backend = self.inner.lock().unwrap();
        backend.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        pos: P,
    ) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.set_cursor_position(pos)
    }

    fn clear(&mut self) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.clear()
    }

    fn clear_region(
        &mut self,
        clear_type: ratatui::backend::ClearType,
    ) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.clear_region(clear_type)
    }

    fn size(&self) -> Result<ratatui::layout::Size, std::io::Error> {
        let backend = self.inner.lock().unwrap();
        backend.size()
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.window_size()
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        let mut backend = self.inner.lock().unwrap();
        backend.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use perry_hermes_agent::{AgentLoop, LoopConfig};
    use perry_hermes_core::compaction_strategy::{
        CompactError, CompactionResult, CompactionStrategy,
    };
    use perry_hermes_core::message::Message;
    use perry_hermes_core::provider::{CompletionStream, Provider};
    use perry_hermes_core::registry::{InMemoryRegistry, ToolSchema};
    use perry_hermes_core::ProviderError;
    use ratatui::layout::Size;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tokio::sync::Mutex as TokioMutex;

    struct NoopProvider;

    #[async_trait]
    impl Provider for NoopProvider {
        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _cancel: CancellationToken,
        ) -> Result<CompletionStream, ProviderError> {
            panic!("manual compact should not call the chat provider")
        }
    }

    struct TestCompactionStrategy;

    #[async_trait]
    impl CompactionStrategy for TestCompactionStrategy {
        async fn compact(
            &mut self,
            _messages: Vec<Message>,
            _focus_topic: Option<&str>,
        ) -> Result<CompactionResult, CompactError> {
            Ok(CompactionResult {
                messages: vec![Message::system("system"), Message::user("summary")],
                summary_usage: perry_hermes_core::Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                },
            })
        }
    }

    fn app_with_context_usage() -> App {
        let mut app = App::new_for_test();
        app.context_used_tokens = Some(90_000);
        app
    }

    #[derive(Clone, Default)]
    struct DrawRecorder {
        calls: Rc<RefCell<Vec<(u16, u16, String)>>>,
    }

    impl Backend for DrawRecorder {
        fn draw<'a, I>(&mut self, content: I) -> Result<(), std::io::Error>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.calls
                .borrow_mut()
                .extend(content.map(|(x, y, cell)| (x, y, cell.symbol().to_string())));
            Ok(())
        }

        fn hide_cursor(&mut self) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn show_cursor(&mut self) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, std::io::Error> {
            Ok(ratatui::layout::Position::new(0, 0))
        }

        fn set_cursor_position<P: Into<ratatui::layout::Position>>(
            &mut self,
            _pos: P,
        ) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn clear(&mut self) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn size(&self) -> Result<Size, std::io::Error> {
            Ok(Size::new(80, 24))
        }

        fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, std::io::Error> {
            Ok(ratatui::backend::WindowSize {
                columns_rows: Size::new(80, 24),
                pixels: Size::new(0, 0),
            })
        }

        fn flush(&mut self) -> Result<(), std::io::Error> {
            Ok(())
        }
    }

    #[test]
    fn wide_cell_safe_backend_skips_hidden_continuation_cells() {
        let recorder = DrawRecorder::default();
        let calls_ref = Rc::clone(&recorder.calls);
        let mut backend = WideCellSafeBackend::new(recorder);
        let mut wide = Cell::new("你");
        let blank = Cell::new(" ");
        let next = Cell::new("好");

        backend
            .draw([(0, 0, &wide), (1, 0, &blank), (2, 0, &next)].into_iter())
            .expect("draw succeeds");

        let calls = calls_ref.borrow();
        assert_eq!(
            *calls,
            vec![(0, 0, "你".to_string()), (2, 0, "好".to_string())]
        );
        drop(calls);

        wide.set_symbol("界");
        backend
            .draw([(0, 1, &wide), (1, 1, &blank)].into_iter())
            .expect("draw succeeds");
        let calls = calls_ref.borrow();
        assert!(
            !calls
                .iter()
                .any(|(x, y, symbol)| *x == 1 && *y == 1 && symbol == " "),
            "continuation blank must not be sent to terminal"
        );
    }

    #[test]
    fn wide_cell_safe_backend_keeps_non_contiguous_cjk_diff_cells() {
        let recorder = DrawRecorder::default();
        let calls_ref = Rc::clone(&recorder.calls);
        let mut backend = WideCellSafeBackend::new(recorder);
        let first = Cell::new("你");
        let second = Cell::new("好");

        backend
            .draw([(0, 0, &first), (2, 0, &second)].into_iter())
            .expect("draw succeeds");

        assert_eq!(
            *calls_ref.borrow(),
            vec![(0, 0, "你".to_string()), (2, 0, "好".to_string())]
        );
    }

    #[tokio::test]
    async fn compact_event_runs_agent_and_replaces_session_messages() {
        let loop_ = AgentLoop::new(
            NoopProvider,
            Arc::new(InMemoryRegistry::new()),
            LoopConfig {
                compaction_strategy: Some(Arc::new(TokioMutex::new(TestCompactionStrategy))),
                ..Default::default()
            },
        );
        let agent = Arc::new(AIAgent::from_loop(loop_));
        let session = AgentSession::new(SessionContext {
            working_dir: PathBuf::from("."),
            session_id: "test".into(),
        });
        session
            .replace_messages(vec![
                Message::user("first request"),
                Message::assistant("first answer"),
                Message::user("middle request"),
                Message::assistant("middle answer"),
                Message::user("latest request"),
            ])
            .await;
        session
            .state
            .remember_first_prompt_context_tokens(1_000)
            .await;
        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let mut app = app_with_context_usage();

        dispatch_event(
            &mut app,
            AppEvent::Compact(None),
            &cancel,
            Some(RunContext {
                agent: &agent,
                session: &session,
                input_tx: &input_tx,
            }),
            None,
        )
        .expect("compact dispatch should start background task");

        let AppEvent::CompactCompleted(result) = input_rx
            .recv()
            .await
            .expect("compact task should send completion")
        else {
            panic!("expected CompactCompleted event");
        };

        dispatch_event(
            &mut app,
            AppEvent::CompactCompleted(result),
            &cancel,
            None,
            None,
        )
        .expect("compact completion should update app state");

        let history = session.messages().await;
        assert_eq!(history.len(), 2);
        assert!(matches!(
            history.get(1),
            Some(Message { content, .. }) if content.as_text() == "summary"
        ));
        assert_eq!(app.context_used_tokens, Some(1_002));
        assert_eq!(app.compression_hint.as_deref(), Some("Compressed in 0ms"));
        assert_eq!(app.mode, AppMode::Idle);
    }

    #[test]
    fn compact_without_agent_does_not_enter_busy_state() {
        let cancel = CancellationToken::new();
        let mut app = app_with_context_usage();

        dispatch_event(&mut app, AppEvent::Compact(None), &cancel, None, None)
            .expect("compact without run context should be handled");

        assert_eq!(app.mode, AppMode::Idle);
        assert_eq!(
            app.compression_hint.as_deref(),
            Some("No agent attached for compact")
        );
    }
}
