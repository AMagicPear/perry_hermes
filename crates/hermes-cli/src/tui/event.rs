//! Internal event types flowing through the TUI's main loop.

use hermes_agent::LoopEvent;

/// A single event consumed by the `App` from any of its event sources.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A raw key press from the terminal.
    Key(crossterm::event::KeyEvent),
    /// A loop event from the agent (translated by the on_event callback).
    Loop(LoopEvent),
    /// A 60 Hz tick used to drive redraws while streaming.
    Tick,
    /// The user pressed Enter and submitted the current input.
    Submit(String),
    /// The user asked to quit (`/quit`, `/exit`, or second Ctrl-C).
    Quit,
    /// The user asked to compact (`/compact [focus]`).
    Compact(Option<String>),
    /// The user asked to clear the scrollback and history (`/clear`).
    Clear,
    /// A rendered line produced by translating a `LoopEvent` (used internally).
    Append(RenderedLine),
    /// Replace the input-box contents (used by `/clear` and similar commands).
    SetInput(String),
}

/// A single line in the chat scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedLine {
    User(String),
    Assistant(String),
    Reasoning(String),
    ToolCall {
        name: String,
        args_preview: String,
    },
    ToolResult {
        name: String,
        output: String,
        ok: bool,
    },
    System(String),
}

/// The TUI's high-level mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Idle,
    AwaitingModel,
    Cancelling,
}