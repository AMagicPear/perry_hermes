//! The TUI's main entry point. A test-friendly `run_with_backend` variant
//! accepts a `TestBackend` and an injected input channel; the production
//! `run` function wraps it with `CrosstermBackend::Stdout`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use hermes_agent::{AIAgent, AgentRunError, SessionContext};
use hermes_core::error::LoopError;
use hermes_core::message::Message;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crate::tui::input::handle_key;
use crate::tui::loop_bridge::apply_loop_event;
use crate::tui::make_on_event;
use crate::tui::render::render;

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
    use crossterm::event::{Event, EventStream, MouseEventKind};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use std::io::stdout;

    enable_raw_mode().map_err(|e| RunError::Tui(e.to_string()))?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .map_err(|e| RunError::Tui(e.to_string()))?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(16));

    let session = SessionContext {
        working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        session_id: "cli".into(),
    };

    let result: Result<(), RunError> = async {
        loop {
            terminal
                .draw(|f| render(f, &app))
                .map_err(|e| RunError::Tui(e.to_string()))?;

            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    app.push_line(RenderedLine::System("⚠ cancelled".to_string()));
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
                            )? {
                                return Ok(());
                            }
                        }
                        Some(Ok(Event::Resize(_, _))) => {
                            // Next tick will redraw at the new size.
                        }
                        Some(Ok(Event::Mouse(mouse))) => {
                            let ev = match mouse.kind {
                                MouseEventKind::ScrollUp if app.mode == AppMode::Idle => {
                                    app.chat_scroll = app.chat_scroll.saturating_add(3);
                                    Some(AppEvent::Tick)
                                }
                                MouseEventKind::ScrollDown if app.mode == AppMode::Idle => {
                                    app.chat_scroll = app.chat_scroll.saturating_sub(3);
                                    Some(AppEvent::Tick)
                                }
                                _ => None,
                            };
                            if let Some(ev) = ev {
                                let _ = dispatch_event(&mut app, ev, &cancel, None)?;
                            }
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
                        if dispatch_event(&mut app, ev, &cancel, None)? {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    .await;

    if let Err(e) = disable_raw_mode() {
        eprintln!("[hermes-cli] warning: failed to disable raw mode: {e}");
    }
    if let Err(e) = execute!(
        stdout(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    ) {
        eprintln!("[hermes-cli] warning: failed to leave alternate screen: {e}");
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
    let backend = SharedTestBackend { inner: backend };
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;

    loop {
        terminal
            .draw(|f| render(f, &app))
            .map_err(|e| RunError::Tui(e.to_string()))?;

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                app.push_line(RenderedLine::System("⚠ cancelled".to_string()));
                return Ok(());
            }
            maybe = input_rx.recv() => {
                let Some(ev) = maybe else { return Ok(()); };
                if dispatch_event(&mut app, ev, &cancel, None)? {
                    return Ok(());
                }
            }
        }
    }
}

struct RunContext<'a> {
    agent: &'a Arc<AIAgent>,
    session: &'a SessionContext,
    input_tx: &'a mpsc::UnboundedSender<AppEvent>,
}

fn dispatch_event(
    app: &mut App,
    ev: AppEvent,
    cancel: &CancellationToken,
    run_ctx: Option<RunContext<'_>>,
) -> Result<bool, RunError> {
    match ev {
        AppEvent::Key(k) => {
            let next = handle_key(app, k);
            dispatch_event(app, next, cancel, run_ctx)
        }
        AppEvent::Loop(loop_ev) => {
            let _ = apply_loop_event(app, loop_ev);
            Ok(false)
        }
        AppEvent::Tick => Ok(false),
        AppEvent::Submit(text) => {
            app.push_line(RenderedLine::User(text.clone()));
            app.session_history.push(Message::user(&text));
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
                let messages = app.session_history.clone();
                let result_tx = ctx.input_tx.clone();
                app.active_turn_cancel = Some(turn_cancel.clone());
                tokio::spawn(async move {
                    let res = agent
                        .run_messages(messages, &session, turn_cancel, on_event)
                        .await;
                    let _ = result_tx.send(AppEvent::TurnCompleted(res));
                });
            }
            Ok(false)
        }
        AppEvent::Quit => Ok(true),
        AppEvent::Compact(focus) => {
            app.push_line(RenderedLine::System(format!(
                "Manual compact requested (focus: {}).",
                focus.as_deref().unwrap_or("(none)")
            )));
            Ok(false)
        }
        AppEvent::Clear => {
            app.scrollback.clear();
            app.session_history.clear();
            app.chat_scroll = 0;
            Ok(false)
        }
        AppEvent::Append(line) => {
            app.push_line(line);
            Ok(false)
        }
        AppEvent::SetInput(s) => {
            app.input = s;
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
                Ok(run_result) => {
                    app.session_history = run_result.messages;
                }
                Err(AgentRunError::Loop(LoopError::CancelledWith(partial))) => {
                    app.session_history.push(partial);
                }
                Err(AgentRunError::Loop(LoopError::Cancelled)) => {}
                Err(e) => {
                    app.push_line(RenderedLine::System(format!("error: {e}")));
                }
            }
            app.mode = AppMode::Idle;
            Ok(false)
        }
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
