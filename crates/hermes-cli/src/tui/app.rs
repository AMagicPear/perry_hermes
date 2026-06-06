//! The TUI's state machine.

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
        }
    }

    /// Push a rendered line into the scrollback.
    pub fn push_line(&mut self, line: RenderedLine) {
        self.scrollback.push(line);
    }
}