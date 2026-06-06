//! The TUI's state machine.

use std::time::Instant;

use hermes_core::message::Message;
use tokio_util::sync::CancellationToken;

use crate::tui::event::{AppMode, RenderedLine};

/// Top-level TUI state. Owned by the event loop in `tui::mod`.
#[derive(Debug, Clone)]
pub struct App {
    /// Chat history (most recent at the end).
    pub scrollback: Vec<RenderedLine>,
    /// Current text in the input box.
    pub input: String,
    /// High-level mode.
    pub mode: AppMode,
    /// Provider kind (e.g. "openai", "anthropic", "echo") for the status bar.
    pub provider_name: Option<String>,
    /// Model name for the status bar.
    pub model_name: Option<String>,
    /// Latest input-token count from the most recent usage event.
    pub last_input_tokens: Option<u64>,
    /// Latest output-token count from the most recent usage event.
    pub last_output_tokens: Option<u64>,
    /// Current iteration number (0 = none yet).
    pub iteration: u32,
    /// Configured max iterations.
    pub max_iterations: u32,
    /// Display hint shown briefly after a compression event.
    pub compression_hint: Option<String>,
    /// Conversation history accumulated across turns.
    pub session_history: Vec<Message>,
    /// `Some(Instant)` while a turn is in flight (`AppMode::AwaitingModel`).
    /// `None` when idle or cancelling. Drives the elapsed-time readout in
    /// the status bar.
    pub turn_started_at: Option<Instant>,
    /// Offset from the bottom of the chat scrollback. `0` means the chat
    /// shows the most recent line; larger values mean the user has scrolled
    /// up. Reset to `0` whenever new content is pushed to the scrollback.
    pub chat_scroll: u16,
    /// Total context window in tokens, if configured. When `None`, the status
    /// bar hides the context segment entirely.
    pub context_window_size: Option<u64>,
    /// Per-turn cancellation handle. Recreated for each submit so a cancelled
    /// turn does not poison future turns.
    pub active_turn_cancel: Option<CancellationToken>,
}

impl App {
    /// Test constructor. Leaves all fields empty / default.
    pub fn new_for_test() -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            mode: AppMode::Idle,
            provider_name: None,
            model_name: None,
            last_input_tokens: None,
            last_output_tokens: None,
            iteration: 0,
            max_iterations: 0,
            compression_hint: None,
            session_history: Vec::new(),
            turn_started_at: None,
            context_window_size: None,
            chat_scroll: 0,
            active_turn_cancel: None,
        }
    }

    /// Push a rendered line into the scrollback. Also resets the chat
    /// scroll to the bottom (so newly-arrived content is visible).
    pub fn push_line(&mut self, line: RenderedLine) {
        self.scrollback.push(line);
        self.chat_scroll = 0;
    }
}
