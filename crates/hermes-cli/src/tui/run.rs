//! The TUI's main entry point. A test-friendly `run_with_backend` variant
//! accepts a `TestBackend` and an injected input channel; the production
//! `run` function wraps it with `CrosstermBackend::Stdout`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use hermes_agent::{AIAgent, AgentRunError, SessionContext};
use hermes_core::message::Message;
use hermes_core::provider::Provider;
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
) -> Result<(), RunError> {
    use crossterm::event::{Event, EventStream};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use std::io::stdout;

    enable_raw_mode().map_err(|e| RunError::Tui(e.to_string()))?;
    execute!(stdout(), EnterAlternateScreen).map_err(|e| RunError::Tui(e.to_string()))?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);

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
                            match next {
                                AppEvent::Submit(text) => {
                                    app.push_line(RenderedLine::User(text.clone()));
                                    app.mode = AppMode::AwaitingModel;
                                    let on_event = make_on_event(input_tx.clone());
                                    let res = agent
                                        .run_messages(
                                            vec![Message::user(&text)],
                                            &session,
                                            cancel.clone(),
                                            on_event,
                                        )
                                        .await;
                                    if let Err(e) = res {
                                        app.push_line(RenderedLine::System(format!("error: {e}")));
                                    }
                                    app.mode = AppMode::Idle;
                                }
                                AppEvent::Quit => return Ok(()),
                                AppEvent::CancelInFlight => {
                                    app.mode = AppMode::Cancelling;
                                    cancel.cancel();
                                }
                                AppEvent::Clear => {
                                    app.scrollback.clear();
                                }
                                AppEvent::Compact(focus) => {
                                    app.push_line(RenderedLine::System(format!(
                                        "Manual compact requested (focus: {}).",
                                        focus.as_deref().unwrap_or("(none)")
                                    )));
                                }
                                _ => {}
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
                    if let Some(AppEvent::Loop(loop_ev)) = maybe {
                        let _ = apply_loop_event(&mut app, loop_ev);
                    }
                }
            }
        }
    }
    .await;

    disable_raw_mode().ok();
    execute!(stdout(), LeaveAlternateScreen).ok();
    result
}

/// Test-friendly entry point. The caller supplies:
/// - the `Backend` (a `TestBackend` in tests)
/// - the `Provider`
/// - a stream of `AppEvent`s (the test enqueues Submit + Quit)
/// - a `CancellationToken`
/// - the provider/model name for the status bar
///
/// Returns when the input channel is closed and the main loop observes no
/// more events.
pub async fn run_with_backend<B: Backend>(
    backend: B,
    _provider: Arc<dyn Provider>,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<(), RunError> {
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);

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
                match ev {
                    AppEvent::Key(k) => {
                        let next = handle_key(&mut app, k);
                        dispatch(&mut app, next);
                    }
                    AppEvent::Loop(loop_ev) => {
                        let _ = apply_loop_event(&mut app, loop_ev);
                    }
                    AppEvent::Tick => {}
                    AppEvent::Submit(text) => {
                        app.push_line(RenderedLine::User(text));
                        app.mode = AppMode::AwaitingModel;
                    }
                    AppEvent::Quit => return Ok(()),
                    AppEvent::Compact(focus) => {
                        app.push_line(RenderedLine::System(format!(
                            "Manual compact requested (focus: {}).",
                            focus.as_deref().unwrap_or("(none)")
                        )));
                    }
                    AppEvent::Clear => {
                        app.scrollback.clear();
                    }
                    AppEvent::Append(line) => app.push_line(line),
                    AppEvent::SetInput(s) => app.input = s,
                    AppEvent::CancelInFlight => {
                        app.mode = AppMode::Cancelling;
                        cancel.cancel();
                    }
                }
            }
        }
    }
}

fn dispatch(_app: &mut App, _ev: AppEvent) {
    // Reserved for the input handler to push derived events back into the queue.
    // For now, `Submit` / `Quit` / `Clear` are produced directly by `handle_key`
    // and consumed by the main loop, so this is a no-op.
}

/// `run_with_backend_and_capture` — a variant that accepts an `Arc<Mutex<TestBackend>>`
/// so the caller retains access to the backend after the loop. The caller clones
/// the `Arc` before passing it; after the function returns the clone is still
/// valid for inspecting the final rendered buffer.
pub async fn run_with_backend_and_capture(
    backend: Arc<Mutex<ratatui::backend::TestBackend>>,
    provider: Arc<dyn Provider>,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<(), RunError> {
    let backend = SharedTestBackend { inner: backend };
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    let _ = provider;

    loop {
        terminal
            .draw(|f| render(f, &app))
            .map_err(|e| RunError::Tui(e.to_string()))?;

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                app.push_line(RenderedLine::System("⚠ cancelled".to_string()));
                break;
            }
            maybe = input_rx.recv() => {
                let Some(ev) = maybe else { break; };
                match ev {
                    AppEvent::Key(k) => {
                        let next = handle_key(&mut app, k);
                        dispatch(&mut app, next);
                    }
                    AppEvent::Loop(loop_ev) => {
                        let _ = apply_loop_event(&mut app, loop_ev);
                    }
                    AppEvent::Tick => {}
                    AppEvent::Submit(text) => {
                        app.push_line(RenderedLine::User(text));
                        app.mode = AppMode::AwaitingModel;
                    }
                    AppEvent::Quit => break,
                    AppEvent::Compact(focus) => {
                        app.push_line(RenderedLine::System(format!(
                            "Manual compact requested (focus: {}).",
                            focus.as_deref().unwrap_or("(none)")
                        )));
                    }
                    AppEvent::Clear => {
                        app.scrollback.clear();
                    }
                    AppEvent::Append(line) => app.push_line(line),
                    AppEvent::SetInput(s) => app.input = s,
                    AppEvent::CancelInFlight => {
                        app.mode = AppMode::Cancelling;
                        cancel.cancel();
                    }
                }
            }
        }
    }

    Ok(())
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
