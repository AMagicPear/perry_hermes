//! The TUI's state machine.

use std::time::Instant;

use hermes_core::message::Message;
use ratatui::text::Line;
use tokio_util::sync::CancellationToken;

use crate::tui::event::{AppMode, RenderedLine};
use crate::tui::render::build_chat_lines;

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
    /// Monotonic revision for scrollback mutations. Used to invalidate the
    /// wrapped chat cache only when content actually changes.
    scrollback_revision: u64,
    /// Cached wrapped chat lines for the most recently rendered width.
    cached_chat_lines: Vec<Line<'static>>,
    /// Width that `cached_chat_lines` was built for.
    cached_chat_width: Option<u16>,
    /// Scrollback revision that `cached_chat_lines` corresponds to.
    cached_chat_revision: u64,
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
            chat_scroll: 0,
            context_window_size: None,
            active_turn_cancel: None,
            scrollback_revision: 0,
            cached_chat_lines: Vec::new(),
            cached_chat_width: None,
            cached_chat_revision: 0,
        }
    }

    /// Push a rendered line into the scrollback. Also resets the chat
    /// scroll to the bottom (so newly-arrived content is visible).
    pub fn push_line(&mut self, line: RenderedLine) {
        self.scrollback.push(line);
        self.chat_scroll = 0;
        self.mark_scrollback_dirty();
    }

    /// Marks the scrollback as changed without altering scroll position. This
    /// is used by streaming updates that mutate the last rendered line in
    /// place.
    pub fn mark_scrollback_dirty(&mut self) {
        self.scrollback_revision = self.scrollback_revision.wrapping_add(1);
    }

    /// Clears chat history and resets scroll state while invalidating any
    /// cached wrapped lines.
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
        self.chat_scroll = 0;
        self.mark_scrollback_dirty();
    }

    /// Returns wrapped chat lines for the given width, rebuilding the cache
    /// only when the terminal width or scrollback revision has changed.
    pub fn chat_lines_for_width(&mut self, width: u16) -> &[Line<'static>] {
        if self.cached_chat_width != Some(width)
            || self.cached_chat_revision != self.scrollback_revision
        {
            self.cached_chat_lines = build_chat_lines(&self.scrollback, width);
            self.cached_chat_width = Some(width);
            self.cached_chat_revision = self.scrollback_revision;
        }
        &self.cached_chat_lines
    }

    #[cfg(test)]
    fn scrollback_revision(&self) -> u64 {
        self.scrollback_revision
    }

    #[cfg(test)]
    fn chat_cache_state(&self) -> (Option<u16>, u64) {
        (self.cached_chat_width, self.cached_chat_revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_cache_only_invalidates_on_content_or_width_change() {
        let mut app = App::new_for_test();
        app.push_line(RenderedLine::Assistant(
            "hello from a somewhat longer assistant message".to_string(),
        ));

        let initial_revision = app.scrollback_revision();
        let initial_lines = app.chat_lines_for_width(20).len();
        let (cached_width, cached_revision) = app.chat_cache_state();
        assert_eq!(cached_width, Some(20));
        assert_eq!(cached_revision, initial_revision);
        assert!(initial_lines > 0);

        app.chat_scroll = 3;
        let scrolled_lines = app.chat_lines_for_width(20).len();
        let (cached_width_after_scroll, cached_revision_after_scroll) = app.chat_cache_state();
        assert_eq!(scrolled_lines, initial_lines);
        assert_eq!(cached_width_after_scroll, Some(20));
        assert_eq!(cached_revision_after_scroll, initial_revision);

        app.chat_lines_for_width(24);
        let (cached_width_after_resize, cached_revision_after_resize) = app.chat_cache_state();
        assert_eq!(cached_width_after_resize, Some(24));
        assert_eq!(cached_revision_after_resize, initial_revision);

        app.push_line(RenderedLine::User("follow-up".to_string()));
        let updated_revision = app.scrollback_revision();
        assert!(updated_revision > initial_revision);
        app.chat_lines_for_width(24);
        let (_, cached_revision_after_content) = app.chat_cache_state();
        assert_eq!(cached_revision_after_content, updated_revision);
    }
}
