//! The TUI's main entry point. A test-friendly `run_with_backend` variant
//! accepts a `TestBackend` and an injected input channel; the production
//! `run` function wraps it with `CrosstermBackend::Stdout`.

use std::sync::{Arc, Mutex};

use hermes_agent::AIAgent;
use hermes_core::provider::Provider;
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crate::tui::input::handle_key;
use crate::tui::loop_bridge::apply_loop_event;
use crate::tui::render::render;

/// Local error type for the TUI run loop. Since `AgentRunError` only has
/// `Loop` and `FailedTurn` variants, we use a simple enum for now.
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

/// Production entry point: drives the TUI against stdout / real keyboard.
pub async fn run(
    _agent: Arc<AIAgent>,
    _cancel: CancellationToken,
    _provider_name: String,
    _model_name: String,
) -> Result<(), RunError> {
    // Stub for Task 12; see Step 6 in the plan.
    unimplemented!("production run() is Task 12; for now use run_with_backend")
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
                    AppEvent::Compact(_) => {
                        // Wired in Task 11.
                    }
                    AppEvent::Clear => {
                        app.scrollback.clear();
                    }
                    AppEvent::Append(line) => app.push_line(line),
                    AppEvent::SetInput(s) => app.input = s,
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
                    AppEvent::Compact(_) => {}
                    AppEvent::Clear => {
                        app.scrollback.clear();
                    }
                    AppEvent::Append(line) => app.push_line(line),
                    AppEvent::SetInput(s) => app.input = s,
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