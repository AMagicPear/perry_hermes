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
    /// Byte offset of the cursor within `input`. Always at a UTF-8 character
    /// boundary. Range: `0 ..= input.len()`.
    pub cursor: usize,
    /// High-level mode.
    pub mode: AppMode,
    /// Provider kind (e.g. "openai", "anthropic", "echo") for the status bar.
    pub provider_name: Option<String>,
    /// Model name for the status bar.
    pub model_name: Option<String>,
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
    /// Current context usage in tokens as reported by the agent loop. Estimated
    /// before the request and replaced by provider-reported usage after it
    /// arrives.
    pub context_used_tokens: Option<u64>,
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
            cursor: 0,
            mode: AppMode::Idle,
            provider_name: None,
            model_name: None,
            iteration: 0,
            max_iterations: 0,
            compression_hint: None,
            session_history: Vec::new(),
            turn_started_at: None,
            chat_scroll: 0,
            context_window_size: None,
            context_used_tokens: None,
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

    // ── Cursor movement & text editing ──────────────────────────────────

    /// Insert a character at the cursor position. Advances the cursor past
    /// the inserted character.
    pub fn insert_at_cursor(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character immediately before the cursor.
    /// Returns `true` if anything was deleted.
    pub fn delete_before_cursor(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let prev = self.prev_char_boundary(self.cursor);
        self.input.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        true
    }

    /// Delete the character at the cursor (forward delete / Delete key).
    /// Returns `true` if anything was deleted.
    pub fn delete_at_cursor(&mut self) -> bool {
        if self.cursor >= self.input.len() {
            return false;
        }
        let next = self.next_char_boundary(self.cursor);
        self.input.replace_range(self.cursor..next, "");
        true
    }

    /// Move the cursor one character to the left. CJK-safe — moves past the
    /// entire multi-byte character.
    pub fn move_cursor_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.prev_char_boundary(self.cursor);
    }

    /// Move the cursor one character to the right. CJK-safe.
    pub fn move_cursor_right(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        self.cursor = self.next_char_boundary(self.cursor);
    }

    /// Move the cursor to the beginning of the input.
    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the input.
    pub fn move_cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Find the byte offset of the previous character boundary before `pos`.
    fn prev_char_boundary(&self, pos: usize) -> usize {
        let mut p = pos.saturating_sub(1);
        while !self.input.is_char_boundary(p) && p > 0 {
            p -= 1;
        }
        p
    }

    /// Find the byte offset of the next character boundary after `pos`.
    fn next_char_boundary(&self, pos: usize) -> usize {
        let mut p = (pos + 1).min(self.input.len());
        while !self.input.is_char_boundary(p) && p < self.input.len() {
            p += 1;
        }
        p
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

    // ── Cursor movement tests ───────────────────────────────────────────

    #[test]
    fn cursor_inserts_at_position() {
        let mut app = App::new_for_test();
        app.input = "helo".to_string();
        app.cursor = 3;
        app.insert_at_cursor('l');
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn cursor_inserts_cjk_at_position() {
        let mut app = App::new_for_test();
        app.input = "你好世".to_string();
        app.cursor = 9;
        app.insert_at_cursor('界');
        assert_eq!(app.input, "你好世界");
        assert_eq!(app.cursor, 12);
    }

    #[test]
    fn cursor_delete_before_cursor_works() {
        let mut app = App::new_for_test();
        app.input = "hello".to_string();
        app.cursor = 5;
        assert!(app.delete_before_cursor());
        assert_eq!(app.input, "hell");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn cursor_delete_before_cursor_noop_at_start() {
        let mut app = App::new_for_test();
        app.input = "hi".to_string();
        app.cursor = 0;
        assert!(!app.delete_before_cursor());
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn cursor_delete_at_cursor_forward() {
        let mut app = App::new_for_test();
        app.input = "abcd".to_string();
        app.cursor = 1;
        assert!(app.delete_at_cursor());
        assert_eq!(app.input, "acd");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn cursor_delete_at_cursor_cjk() {
        let mut app = App::new_for_test();
        app.input = "你好啊".to_string();
        app.cursor = 3;
        assert!(app.delete_at_cursor());
        assert_eq!(app.input, "你啊");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn cursor_moves_left_and_right_on_ascii() {
        let mut app = App::new_for_test();
        app.input = "abc".to_string();
        app.cursor = 3;
        app.move_cursor_left();
        assert_eq!(app.cursor, 2);
        app.move_cursor_left();
        assert_eq!(app.cursor, 1);
        app.move_cursor_right();
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn cursor_moves_left_and_right_on_cjk() {
        let mut app = App::new_for_test();
        app.input = "你好".to_string();
        app.cursor = 6;
        app.move_cursor_left();
        assert_eq!(app.cursor, 3);
        app.move_cursor_left();
        assert_eq!(app.cursor, 0);
        app.move_cursor_right();
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn cursor_home_and_end() {
        let mut app = App::new_for_test();
        app.input = "hello".to_string();
        app.cursor = 3;
        app.move_cursor_home();
        assert_eq!(app.cursor, 0);
        app.move_cursor_end();
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn cursor_stays_in_bounds_at_edges() {
        let mut app = App::new_for_test();
        app.input = "ab".to_string();
        app.cursor = 0;
        app.move_cursor_left();
        assert_eq!(app.cursor, 0);
        app.cursor = 2;
        app.move_cursor_right();
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn empty_input_cursor_operations_are_noops() {
        let mut app = App::new_for_test();
        assert!(!app.delete_before_cursor());
        assert!(!app.delete_at_cursor());
        app.move_cursor_left();
        assert_eq!(app.cursor, 0);
        app.move_cursor_right();
        assert_eq!(app.cursor, 0);
        app.move_cursor_home();
        assert_eq!(app.cursor, 0);
        app.move_cursor_end();
        assert_eq!(app.cursor, 0);
    }
}
