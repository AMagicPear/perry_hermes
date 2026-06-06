//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod render;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};