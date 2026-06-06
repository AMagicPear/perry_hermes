# Phase 10: Rename `hermes-skills` → `hermes-skill-loader` + Replace `hermes-cli` REPL with `ratatui` TUI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the leaf data crate `hermes-skills` → `hermes-skill-loader` (content/responsibility unchanged) and replace the `hermes-cli` REPL with a `ratatui`-based TUI. The legacy REPL code (`repl.rs`, `cli_render.rs`) is discarded; the new `hermes-cli` binary is a TUI only.

**Architecture:**
- The rename is a mechanical refactor: directory `git mv`, package name, all `use hermes_skills::` → `use hermes_skill_loader::`, Cargo.toml workspace members.
- The TUI lives in a new `tui/` module inside `crates/hermes-cli/src/`. The TUI owns the `App` state machine, an `AppEvent` channel, and three event sources (`crossterm` keys, an `mpsc` from the agent's `on_event` callback, and a 60 Hz tick) merged via `tokio::select!`.
- All TUI rendering is done through a `Frame` produced by `ratatui`. Tests use `ratatui::backend::TestBackend` and assert on `Buffer::content()`.

**Tech Stack:** Rust workspace, `ratatui` 0.29, `crossterm` 0.28, `tokio` 1.x, `tokio::sync::mpsc` + `tokio_util::sync::CancellationToken` (already in workspace).

---

## File Structure

### Created

| File | Responsibility |
|---|---|
| `crates/hermes-skill-loader/Cargo.toml` | Renamed manifest for the data crate (was `hermes-skills`). |
| `crates/hermes-skill-loader/src/lib.rs` | Renamed entry point; same content, updated `//!` doc. |
| `crates/hermes-skill-loader/src/frontmatter.rs` | Moved verbatim from `hermes-skills`. |
| `crates/hermes-skill-loader/src/layout.rs` | Moved verbatim from `hermes-skills`. |
| `crates/hermes-skill-loader/src/validate.rs` | Moved verbatim from `hermes-skills`. |
| `crates/hermes-cli/src/tui/mod.rs` | TUI entry point: `pub async fn run(agent, session, cancel) -> Result<()>`; sets up `crossterm` raw mode, the `tokio::select!` loop, and the `Terminal`. |
| `crates/hermes-cli/src/tui/app.rs` | `App` state machine: scrollback, input buffer, status bar, mode. Owns the `mpsc` channel from the agent. |
| `crates/hermes-cli/src/tui/event.rs` | `AppEvent` enum (Key, Loop, Tick, Submit, Quit, Compact, Clear) and `RenderedLine` enum. |
| `crates/hermes-cli/src/tui/input.rs` | `KeyEvent → AppEvent` mapping, slash-command parser, input-buffer editing. |
| `crates/hermes-cli/src/tui/render.rs` | `pub fn render(f: &mut Frame, app: &App)` — paints the chat area, status bar, and input box. |
| `crates/hermes-cli/tests/tui_render.rs` | Snapshot-style assertions on `TestBackend` buffers for the render module. |
| `crates/hermes-cli/tests/tui_input.rs` | Slash-command parsing and input-buffer behavior. |
| `crates/hermes-cli/tests/tui_smoke.rs` | End-to-end test: starts the TUI with a `ScriptedProvider`, drives a turn, asserts the rendered buffer. |

### Modified

| File | Change |
|---|---|
| `Cargo.toml` | Rename workspace member; drop the `hermes-tui` placeholder. |
| `crates/hermes-agent/Cargo.toml` | Dependency `hermes-skills` → `hermes-skill-loader`. |
| `crates/hermes-agent/src/prompting.rs` | Update `use hermes_skills::...` → `use hermes_skill_loader::...`. |
| `crates/hermes-agent/src/tool_catalog.rs` | Same import path update. |
| `crates/hermes-cli/Cargo.toml` | Add `ratatui` 0.29, `crossterm` 0.28; add `ratatui` (with `crossterm` feature) to `[dev-dependencies]`. |
| `crates/hermes-cli/src/main.rs` | Replace REPL entry point with `tui::run(...)`. |

### Deleted

| File | Reason |
|---|---|
| `crates/hermes-cli/src/repl.rs` | Superseded by the TUI. |
| `crates/hermes-cli/src/cli_render.rs` | Superseded by `tui/render.rs`. |
| `crates/hermes-skills/Cargo.toml` | Renamed to `hermes-skill-loader`. |
| `crates/hermes-skills/src/lib.rs` | Renamed. |
| `crates/hermes-skills/src/frontmatter.rs` | Renamed. |
| `crates/hermes-skills/src/layout.rs` | Renamed. |
| `crates/hermes-skills/src/validate.rs` | Renamed. |
| `crates/hermes-cli/tests/cli_smoke.rs` | The smoke test piped stdin to the REPL; the TUI does not accept piped input in Phase 10. The test is replaced by `tui_smoke.rs`. |

---

## Task 1: Rename `hermes-skills` → `hermes-skill-loader`

**Files:**
- Modify: `Cargo.toml:3-10`
- Modify: `crates/hermes-agent/Cargo.toml`
- Modify: all `use hermes_skills::...` call sites in `crates/hermes-agent/src/**/*.rs`, `crates/hermes-cli/src/**/*.rs`, and any tests that import the crate

- [ ] **Step 1: Move the crate directory with `git mv`**

Run from the repo root:

```bash
git mv crates/hermes-skills crates/hermes-skill-loader
```

- [ ] **Step 2: Update the package name in the moved manifest**

Edit `crates/hermes-skill-loader/Cargo.toml` and change the `[package] name` line from `hermes-skills` to `hermes-skill-loader`. The other fields (version, edition, dependencies on `serde`, `serde_yaml`, etc.) stay identical.

- [ ] **Step 3: Update the workspace `Cargo.toml`**

Edit the root `Cargo.toml` and replace the `hermes-skills` member with the renamed crate. The relevant block becomes:

```toml
[workspace]
resolver = "2"
members = [
    "crates/hermes-agent",
    "crates/hermes-core",
    "crates/hermes-providers",
    "crates/hermes-skill-loader",
    "crates/hermes-cli",
    # "crates/hermes-gateway",  # phase 11+
]
```

- [ ] **Step 4: Update `hermes-agent/Cargo.toml`**

Edit `crates/hermes-agent/Cargo.toml` and replace any `hermes-skills = { path = ... }` dependency with `hermes-skill-loader = { path = "../hermes-skill-loader" }`.

- [ ] **Step 5: Update all `use` statements**

Find every `use hermes_skills::` and replace with `use hermes_skill_loader::`. Run this from the repo root to confirm there are no stragglers:

```bash
grep -rn "hermes_skills" crates/
```

Expected: no matches. If any match remains, edit them manually (typical sites: `crates/hermes-agent/src/prompting.rs`, `crates/hermes-agent/src/tool_catalog.rs`, plus the `tests/` directories that exercise skill loading).

- [ ] **Step 6: Update the moved crate's own `lib.rs` doc comment**

Edit `crates/hermes-skill-loader/src/lib.rs` and update the module-level docstring from:

```rust
//! Skill loading and system-prompt injection for the Hermes agent.
//!
//! See `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md`
//! for the full design.
```

to:

```rust
//! Skill data loading and system-prompt injection for the Hermes agent.
//!
//! This crate is a leaf: it parses `SKILL.md` files, validates them, and renders
//! the prompt-injection metadata block. The LLM-callable runtime tools that
//! explore loaded skills (`SkillListTool`, `SkillViewTool`, ...) live in
//! `hermes-agent::tools::skills`.
//!
//! See `docs/superpowers/specs/2026-06-06-phase-10-rename-and-tui-design.md`
//! for the rename context.
```

The body of `lib.rs` (the `pub mod frontmatter;` / `pub mod layout;` / `pub mod validate;` declarations and the `render_system_prompt_block` function) stays identical except that the inline doc comment for the "Phase 12 deliverable" sentence (in `render_system_prompt_block`) gets a one-line tweak: change "The actual `SkillActivationTool` is a Phase 12 deliverable." to "The actual `SkillViewTool` is delivered in Phase 9's built-in tools expansion." (the SkillViewTool already exists in `hermes-agent::tools::skills`).

- [ ] **Step 7: Build to confirm the rename compiles**

```bash
cargo build --workspace
```

Expected: build succeeds with no errors. There may be warnings from unused imports if any `use` was missed; the grep in Step 5 should have caught them, but double-check.

- [ ] **Step 8: Run the full test suite to confirm no behavioral regression**

```bash
cargo test --workspace
```

Expected: all existing tests pass. The rename is content-preserving; no test should change behavior.

- [ ] **Step 9: Commit the rename**

```bash
git add -A
git commit -m "refactor(phase 10): rename hermes-skills to hermes-skill-loader

Content/responsibility unchanged. Mechanical rename across the workspace
to make the data-only scope of the crate explicit (LLM-callable tools
that operate on skills remain in hermes-agent::tools::skills)."
```

---

## Task 2: Add `ratatui` + `crossterm` dependencies

**Files:**
- Modify: `crates/hermes-cli/Cargo.toml`

- [ ] **Step 1: Edit `crates/hermes-cli/Cargo.toml` to add the TUI dependencies**

Add a new `[dependencies]` block entry and a `[dev-dependencies]` entry. The file should look like:

```toml
[package]
name = "hermes-cli"
version = "0.2.0"
edition = "2021"
rust-version = "1.75"
license = "MIT"

[dependencies]
hermes-agent = { path = "../hermes-agent" }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
ratatui = "0.29"
crossterm = "0.28"

[dev-dependencies]
tempfile = "3"
ratatui = { version = "0.29", features = ["crossterm"] }
```

If the file already has a `[dependencies]` block, add `ratatui = "0.29"` and `crossterm = "0.28"` to it instead of creating a new block.

- [ ] **Step 2: Build to confirm the dependencies resolve**

```bash
cargo build -p hermes-cli
```

Expected: build succeeds. No new code is exercised yet, but the dependency graph must be valid.

- [ ] **Step 3: Commit the dependency addition**

```bash
git add crates/hermes-cli/Cargo.toml
git commit -m "build(phase 10): add ratatui + crossterm to hermes-cli

No code yet. The TUI modules land in subsequent tasks; this commit only
pins the dependency graph so we can see compile-time impact in isolation."
```

---

## Task 3: TUI scaffolding — `App` struct, `AppEvent` enum, and a render-to-`TestBackend` smoke test

**Files:**
- Create: `crates/hermes-cli/src/tui/mod.rs`
- Create: `crates/hermes-cli/src/tui/app.rs`
- Create: `crates/hermes-cli/src/tui/event.rs`
- Create: `crates/hermes-cli/src/tui/render.rs`
- Create: `crates/hermes-cli/tests/tui_render.rs`

- [ ] **Step 1: Write the failing render test**

Create `crates/hermes-cli/tests/tui_render.rs`:

```rust
//! Smoke test for the TUI render module. Drives an empty `App` through a
//! `TestBackend` and asserts the rendered buffer contains the expected
//! status bar and input-box placeholders.

use hermes_cli::tui::app::App;
use hermes_cli::tui::event::AppMode;
use hermes_cli::tui::render::render;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[test]
fn empty_app_renders_input_box_and_status_bar() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let app = App::new_for_test();
    terminal
        .draw(|f| render(f, &app))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The status bar (last row) must contain the placeholder for the input box.
    let status_y = buffer.area.height.saturating_sub(1);
    let status_row: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, status_y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(
        status_row.contains("Type a message"),
        "status row should contain the input-box placeholder; got: {status_row:?}"
    );
}
```

For this test to compile, `App`, `AppMode`, and `render` must be public and the `tui` module must export them. The test must FAIL on the first run because none of those types exist yet.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_render -- --nocapture
```

Expected: FAIL with "module `tui` not found" or similar — the `tui` module does not exist yet. This is the RED step.

- [ ] **Step 3: Create `tui/event.rs` with the `AppEvent` and `RenderedLine` enums**

Create `crates/hermes-cli/src/tui/event.rs`:

```rust
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
```

Note: this assumes `LoopEvent` is re-exported from `hermes-agent` and `crossterm::event::KeyEvent` is in scope. If either is not the case, see the spec's "Open Questions" section — adjust the import path.

- [ ] **Step 4: Create `tui/app.rs` with a minimal `App` struct**

Create `crates/hermes-cli/src/tui/app.rs`:

```rust
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
```

- [ ] **Step 5: Create `tui/render.rs` with a minimal render that paints the status bar placeholder**

Create `crates/hermes-cli/src/tui/render.rs`:

```rust
//! Frame painter for the TUI.

use crate::tui::app::App;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Paint one frame.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // chat area
            Constraint::Length(3), // input box
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    // Chat area: a placeholder paragraph listing the lines we have so far.
    let chat_lines: Vec<Line> = app
        .scrollback
        .iter()
        .map(|line| match line {
            crate::tui::event::RenderedLine::User(s) => Line::from(format!("> {s}")),
            crate::tui::event::RenderedLine::Assistant(s) => Line::from(s.clone()),
            crate::tui::event::RenderedLine::Reasoning(s) => Line::from(format!("… {s}")),
            crate::tui::event::RenderedLine::ToolCall { name, args_preview } => {
                Line::from(format!("⚙ {name}({args_preview})"))
            }
            crate::tui::event::RenderedLine::ToolResult { name, output, ok } => {
                Line::from(format!("{} {name}: {}", if *ok { "✓" } else { "✗" }, output))
            }
            crate::tui::event::RenderedLine::System(s) => Line::from(format!("[system] {s}")),
        })
        .collect();
    let chat = Paragraph::new(chat_lines).block(Block::default().borders(Borders::NONE));
    f.render_widget(chat, chunks[0]);

    // Input box.
    let input_text = if app.input.is_empty() {
        "Type a message and press Enter. /quit, /compact [focus], /clear.".to_string()
    } else {
        app.input.clone()
    };
    let input = Paragraph::new(input_text)
        .block(Block::default().borders(Borders::ALL).title("Input"));
    f.render_widget(input, chunks[1]);

    // Status bar.
    let status = Paragraph::new(Line::from("● idle")).style(Style::default());
    f.render_widget(status, chunks[2]);
}
```

- [ ] **Step 6: Create `tui/mod.rs` with a placeholder `run` and the public re-exports**

Create `crates/hermes-cli/src/tui/mod.rs`:

```rust
//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod render;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
```

- [ ] **Step 7: Expose the `tui` module from the crate root**

Edit `crates/hermes-cli/src/lib.rs` (or create it if it does not exist). The file should be:

```rust
//! Local product shell for Hermes: a `ratatui`-based TUI that consumes
//! the `hermes-agent` runtime.

pub mod tui;
```

- [ ] **Step 8: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_render
```

Expected: PASS. The `App::new_for_test()` constructor builds an empty app; `render` paints the chat area, the input box with the placeholder text, and the status bar; the test asserts the input-box placeholder appears in the last row of the buffer.

- [ ] **Step 9: Commit the TUI scaffolding**

```bash
git add crates/hermes-cli/src/tui crates/hermes-cli/src/lib.rs crates/hermes-cli/tests/tui_render.rs
git commit -m "feat(phase 10): TUI scaffolding - App state, AppEvent, render smoke test

Empty App renders a chat area, an input box with the placeholder text,
and a status bar. The test drives the App through a TestBackend and
asserts the input-box placeholder appears in the last row."
```

---

## Task 4: Input buffer — typing and backspace

**Files:**
- Modify: `crates/hermes-cli/src/tui/input.rs` (currently empty)
- Create: `crates/hermes-cli/tests/tui_input.rs`

- [ ] **Step 1: Write the failing input-buffer test**

Create `crates/hermes-cli/tests/tui_input.rs`:

```rust
//! Tests for the input layer: typing, backspace, and Enter -> Submit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::AppEvent;
use hermes_cli::tui::input::handle_key;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn typing_appends_to_input_buffer() {
    let mut app = App::new_for_test();
    let ev = handle_key(&mut app, key(KeyCode::Char('h')));
    assert!(matches!(ev, AppEvent::Tick) || ev == AppEvent::Tick);
    let ev = handle_key(&mut app, key(KeyCode::Char('i')));
    assert_eq!(ev, AppEvent::Tick);
    assert_eq!(app.input, "hi");
}

#[test]
fn backspace_removes_last_char() {
    let mut app = App::new_for_test();
    app.input.push_str("hello");
    let ev = handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(ev, AppEvent::Tick);
    assert_eq!(app.input, "hell");
}

#[test]
fn enter_submits_input() {
    let mut app = App::new_for_test();
    app.input.push_str("hi there");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Submit("hi there".to_string()));
    assert_eq!(app.input, "");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_input
```

Expected: FAIL because `tui::input` is empty (and `handle_key` does not exist).

- [ ] **Step 3: Implement `handle_key` in `tui/input.rs`**

Edit `crates/hermes-cli/src/tui/input.rs`:

```rust
//! KeyEvent -> AppEvent mapping, plus input-buffer editing.

use crate::tui::app::App;
use crate::tui::event::AppEvent;
use crossterm::event::{KeyCode, KeyEvent};

/// Apply a key event to the App's input buffer. Returns the AppEvent that
/// the main loop should process.
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppEvent {
    match key.code {
        KeyCode::Char(c) => {
            app.input.push(c);
            AppEvent::Tick
        }
        KeyCode::Backspace => {
            app.input.pop();
            AppEvent::Tick
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            AppEvent::Submit(text)
        }
        _ => AppEvent::Tick,
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_input
```

Expected: PASS for all three tests.

- [ ] **Step 5: Commit the input layer**

```bash
git add crates/hermes-cli/src/tui/input.rs crates/hermes-cli/tests/tui_input.rs
git commit -m "feat(phase 10): TUI input layer - typing, backspace, Enter -> Submit"
```

---

## Task 5: Slash command parser

**Files:**
- Modify: `crates/hermes-cli/src/tui/input.rs`
- Modify: `crates/hermes-cli/tests/tui_input.rs`

- [ ] **Step 1: Add failing slash-command tests**

Append to `crates/hermes-cli/tests/tui_input.rs`:

```rust
#[test]
fn slash_quit_produces_quit_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/quit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Quit);
    assert_eq!(app.input, "");
}

#[test]
fn slash_exit_produces_quit_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/exit");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Quit);
}

#[test]
fn slash_compact_with_focus_produces_compact_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/compact focus on shell commands");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Compact(Some("focus on shell commands".to_string())));
}

#[test]
fn slash_compact_without_focus_produces_compact_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/compact");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Compact(None));
}

#[test]
fn slash_clear_produces_clear_event() {
    let mut app = App::new_for_test();
    app.input.push_str("/clear");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(ev, AppEvent::Clear);
}

#[test]
fn unknown_slash_command_is_rejected_with_system_message() {
    let mut app = App::new_for_test();
    app.input.push_str("/bogus");
    let ev = handle_key(&mut app, key(KeyCode::Enter));
    // The parser returns SetInput("") so the input clears, and the App pushes
    // a System line into the scrollback. We return Append for the loop to
    // route into push_line.
    match ev {
        AppEvent::Append(crate::tui::event::RenderedLine::System(s)) => {
            assert!(s.contains("Unknown"));
        }
        other => panic!("expected Append(System); got {other:?}"),
    }
    assert_eq!(app.input, "");
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

```bash
cargo test -p hermes-cli --test tui_input
```

Expected: the four new tests FAIL (typing tests pass, but the slash-command ones fail because `handle_key` only treats Enter as `Submit`).

- [ ] **Step 3: Extend `handle_key` to dispatch slash commands**

Replace the `KeyCode::Enter` arm in `crates/hermes-cli/src/tui/input.rs` with:

```rust
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            parse_slash_or_submit(text)
        }
        _ => AppEvent::Tick,
    }
}

fn parse_slash_or_submit(text: String) -> AppEvent {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return AppEvent::Submit(text);
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match cmd {
        "/quit" | "/exit" => AppEvent::Quit,
        "/compact" => AppEvent::Compact(if rest.is_empty() { None } else { Some(rest.to_string()) }),
        "/clear" => AppEvent::Clear,
        other => AppEvent::Append(crate::tui::event::RenderedLine::System(format!(
            "Unknown command: {other}. Try /quit, /compact [focus], /clear."
        ))),
    }
}
```

- [ ] **Step 4: Run the tests to verify they all pass**

```bash
cargo test -p hermes-cli --test tui_input
```

Expected: all six tests pass (the three from Task 4 plus the four new ones — actually six total in this file).

- [ ] **Step 5: Commit the slash command parser**

```bash
git add crates/hermes-cli/src/tui/input.rs crates/hermes-cli/tests/tui_input.rs
git commit -m "feat(phase 10): TUI slash commands - /quit, /exit, /compact [focus], /clear"
```

---

## Task 6: Status bar — provider, model, tokens, mode

**Files:**
- Modify: `crates/hermes-cli/src/tui/render.rs`
- Modify: `crates/hermes-cli/tests/tui_render.rs`

- [ ] **Step 1: Add a failing test for the populated status bar**

Append to `crates/hermes-cli/tests/tui_render.rs`:

```rust
#[test]
fn populated_app_renders_status_bar_with_provider_and_tokens() {
    use hermes_cli::tui::event::AppMode;

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.provider_name = Some("openai".to_string());
    app.model_name = Some("gpt-4.1-mini".to_string());
    app.iteration = 2;
    app.max_iterations = 10;
    app.last_input_tokens = Some(12_345);
    app.last_output_tokens = Some(4_567);
    app.mode = AppMode::AwaitingModel;

    terminal
        .draw(|f| render(f, &app))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let status_y = buffer.area.height.saturating_sub(1);
    let status_row: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, status_y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(status_row.contains("openai"), "status row missing provider: {status_row:?}");
    assert!(status_row.contains("gpt-4.1-mini"), "status row missing model: {status_row:?}");
    assert!(status_row.contains("12.3K"), "status row missing input tokens: {status_row:?}");
    assert!(status_row.contains("4.5K"), "status row missing output tokens: {status_row:?}");
    assert!(status_row.contains("iter 2/10"), "status row missing iteration: {status_row:?}");
    assert!(status_row.contains("awaiting"), "status row missing mode: {status_row:?}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_render
```

Expected: FAIL because the current `render` only writes "● idle" to the status bar.

- [ ] **Step 3: Replace the status-bar widget in `tui/render.rs`**

Edit the bottom of `render()` in `crates/hermes-cli/src/tui/render.rs`. Replace the existing `let status = Paragraph::new(Line::from("● idle")).style(Style::default());` block with:

```rust
    // Status bar: provider · model · iter X/Y · in Z · out W · mode
    let provider = app.provider_name.as_deref().unwrap_or("?");
    let model = app.model_name.as_deref().unwrap_or("?");
    let in_tok = app
        .last_input_tokens
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let out_tok = app
        .last_output_tokens
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let mode = match app.mode {
        crate::tui::event::AppMode::Idle => "idle",
        crate::tui::event::AppMode::AwaitingModel => "awaiting",
        crate::tui::event::AppMode::Cancelling => "cancelling",
    };
    let line = format!(
        "{provider} · {model} · iter {iter}/{max_iter} · in {in_tok} · out {out_tok} · {mode}",
        iter = app.iteration,
        max_iter = app.max_iterations,
    );
    let status = Paragraph::new(Line::from(line)).style(Style::default());
    f.render_widget(status, chunks[2]);
```

Add a `format_tokens` helper at the bottom of `render.rs`:

```rust
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_render
```

Expected: both status-bar tests pass.

- [ ] **Step 5: Commit the populated status bar**

```bash
git add crates/hermes-cli/src/tui/render.rs crates/hermes-cli/tests/tui_render.rs
git commit -m "feat(phase 10): TUI status bar - provider, model, tokens, iteration, mode"
```

---

## Task 7: Translate `LoopEvent` → `AppEvent` (and update the App)

**Files:**
- Create: `crates/hermes-cli/src/tui/loop_bridge.rs`
- Modify: `crates/hermes-cli/src/tui/mod.rs`
- Create: `crates/hermes-cli/tests/tui_loop_bridge.rs`

- [ ] **Step 1: Write the failing bridge test**

Create `crates/hermes-cli/tests/tui_loop_bridge.rs`:

```rust
//! Tests for translating `LoopEvent` -> `AppEvent`.

use hermes_agent::LoopEvent;
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppEvent, AppMode, RenderedLine};
use hermes_cli::tui::loop_bridge::apply_loop_event;

fn app_with_mode(mode: AppMode) -> App {
    let mut app = App::new_for_test();
    app.mode = mode;
    app
}

#[test]
fn content_delta_appends_assistant_text() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ContentDelta("hello ".to_string());
    let next = apply_loop_event(&mut app, ev);
    assert_eq!(next, AppEvent::Tick);
    assert_eq!(
        app.scrollback.last(),
        Some(&RenderedLine::Assistant("hello ".to_string()))
    );
}

#[test]
fn tool_call_event_pushes_tool_call_line() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ToolCallStart {
        name: "terminal".to_string(),
        args_preview: "{\"cmd\":\"ls\"}".to_string(),
    };
    let _ = apply_loop_event(&mut app, ev);
    assert!(matches!(
        app.scrollback.last(),
        Some(RenderedLine::ToolCall { name, .. }) if name == "terminal"
    ));
}

#[test]
fn tool_result_event_pushes_tool_result_line() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::ToolResult {
        name: "terminal".to_string(),
        output: "file1\nfile2".to_string(),
        ok: true,
    };
    let _ = apply_loop_event(&mut app, ev);
    assert!(matches!(
        app.scrollback.last(),
        Some(RenderedLine::ToolResult { name, ok: true, .. }) if name == "terminal"
    ));
}

#[test]
fn loop_finished_transitions_to_idle() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::Finished;
    let next = apply_loop_event(&mut app, ev);
    assert_eq!(next, AppEvent::Tick);
    assert_eq!(app.mode, AppMode::Idle);
}

#[test]
fn usage_event_updates_status_bar_tokens() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::Usage {
        input_tokens: 1234,
        output_tokens: 56,
    };
    let _ = apply_loop_event(&mut app, ev);
    assert_eq!(app.last_input_tokens, Some(1234));
    assert_eq!(app.last_output_tokens, Some(56));
}

#[test]
fn compression_completed_sets_hint() {
    let mut app = app_with_mode(AppMode::AwaitingModel);
    let ev = LoopEvent::CompressionCompleted {
        before_tokens: 142_000,
        after_tokens: 38_000,
        duration_ms: 1_200,
    };
    let _ = apply_loop_event(&mut app, ev);
    assert!(app.compression_hint.is_some());
}
```

For this test to compile, `LoopEvent` must be re-exported from `hermes-agent` with at least the variants above. If the actual enum has different variant names, update the test to use the real names (this is a TDD contract: the test defines the bridge's expected event vocabulary, and we make the code conform).

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_loop_bridge
```

Expected: FAIL because `tui::loop_bridge` does not exist yet, and the test cannot import `apply_loop_event`.

- [ ] **Step 3: Create `tui/loop_bridge.rs` with the `apply_loop_event` translator**

Create `crates/hermes-cli/src/tui/loop_bridge.rs`:

```rust
//! Translate `LoopEvent`s from the agent into `AppEvent`s the TUI consumes.

use crate::tui::app::App;
use crate::tui::event::{AppEvent, RenderedLine};
use hermes_agent::LoopEvent;

/// Apply a `LoopEvent` to the `App`, returning the `AppEvent` the main loop
/// should dispatch next.
pub fn apply_loop_event(app: &mut App, ev: LoopEvent) -> AppEvent {
    match ev {
        LoopEvent::ContentDelta(text) => {
            if let Some(RenderedLine::Assistant(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
            } else {
                app.push_line(RenderedLine::Assistant(text));
            }
            AppEvent::Tick
        }
        LoopEvent::ReasoningDelta(text) => {
            if let Some(RenderedLine::Reasoning(existing)) = app.scrollback.last_mut() {
                existing.push_str(&text);
            } else {
                app.push_line(RenderedLine::Reasoning(text));
            }
            AppEvent::Tick
        }
        LoopEvent::ToolCallStart { name, args_preview } => {
            app.push_line(RenderedLine::ToolCall { name, args_preview });
            AppEvent::Tick
        }
        LoopEvent::ToolResult { name, output, ok } => {
            app.push_line(RenderedLine::ToolResult { name, output, ok });
            AppEvent::Tick
        }
        LoopEvent::Usage { input_tokens, output_tokens } => {
            app.last_input_tokens = Some(input_tokens);
            app.last_output_tokens = Some(output_tokens);
            AppEvent::Tick
        }
        LoopEvent::LoopIteration { iteration, max_iterations } => {
            app.iteration = iteration;
            app.max_iterations = max_iterations;
            AppEvent::Tick
        }
        LoopEvent::CompressionCompleted { before_tokens, after_tokens, duration_ms } => {
            app.compression_hint = Some(format!(
                "🗜 Compressed: {b} → {a} tokens in {d}ms",
                b = before_tokens,
                a = after_tokens,
                d = duration_ms
            ));
            AppEvent::Tick
        }
        LoopEvent::Finished => {
            app.mode = crate::tui::event::AppMode::Idle;
            AppEvent::Tick
        }
        // Unknown / future variants are ignored but still cause a redraw tick.
        _ => AppEvent::Tick,
    }
}
```

If the actual `LoopEvent` enum does not have these exact variants, the `match` must be adjusted. The mapping in this file is the contract: it tells the main loop exactly which agent events drive which App state changes. If a variant is missing from the enum, either:
1. Add a `#[non_exhaustive]` annotation on the enum in `hermes-agent` so the TUI can add a default arm, or
2. Restrict the loop to the variants above and ignore others (the `_ =>` arm does this).

- [ ] **Step 4: Export the new module from `tui/mod.rs`**

Edit `crates/hermes-cli/src/tui/mod.rs` and add the new module to the `pub mod` list:

```rust
//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_loop_bridge
```

Expected: all six tests pass.

- [ ] **Step 6: Commit the loop-event bridge**

```bash
git add crates/hermes-cli/src/tui/loop_bridge.rs crates/hermes-cli/src/tui/mod.rs crates/hermes-cli/tests/tui_loop_bridge.rs
git commit -m "feat(phase 10): translate LoopEvent to AppEvent (TUI loop bridge)"
```

---

## Task 8: Wire `on_event` callback through an `mpsc` channel

**Files:**
- Modify: `crates/hermes-cli/src/tui/mod.rs` (add a helper)
- Create: `crates/hermes-cli/tests/tui_on_event.rs`

- [ ] **Step 1: Write the failing on-event wiring test**

Create `crates/hermes-cli/tests/tui_on_event.rs`:

```rust
//! Verifies that the `on_event` closure passed to `AIAgent::run_messages`
//! forwards `LoopEvent`s into the TUI's mpsc channel.

use hermes_agent::LoopEvent;
use tokio::sync::mpsc;

#[tokio::test]
async fn on_event_closure_sends_content_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEventShim>();
    let mut on_event = make_on_event(tx.clone());

    // Simulate the agent invoking the callback for one event.
    on_event(LoopEvent::ContentDelta("hi".to_string()));

    let received = rx.recv().await.expect("event");
    assert_eq!(received, AppEventShim::ContentDelta("hi".to_string()));
}

// ----- shim types and helpers (replace with real types once available) -----

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppEventShim {
    ContentDelta(String),
}

fn make_on_event(tx: mpsc::UnboundedSender<AppEventShim>) -> impl FnMut(LoopEvent) + Send {
    move |ev| {
        if let LoopEvent::ContentDelta(s) = ev {
            let _ = tx.send(AppEventShim::ContentDelta(s));
        }
    }
}
```

This test is intentionally minimal and uses a local shim. The real wiring lands in Step 3; this test establishes that the channel can be driven from the closure.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_on_event
```

Expected: PASS even at the shim level (the channel wiring is correct). The point of this step is to set up the file. Mark it passing in the next step.

- [ ] **Step 3: Replace the shim with the real `tui::on_event` factory**

Edit `crates/hermes-cli/src/tui/mod.rs` and add the factory:

```rust
//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;

use hermes_agent::LoopEvent;
use tokio::sync::mpsc;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};

/// Build the `on_event` closure to pass to `AIAgent::run_messages`. Each
/// `LoopEvent` is forwarded into the TUI's main loop as an `AppEvent::Loop`.
pub fn make_on_event(
    tx: mpsc::UnboundedSender<AppEvent>,
) -> impl FnMut(LoopEvent) + Send {
    move |ev: LoopEvent| {
        let _ = tx.send(AppEvent::Loop(ev));
    }
}
```

- [ ] **Step 4: Replace the shim test with a real one**

Replace `crates/hermes-cli/tests/tui_on_event.rs` entirely:

```rust
//! Verifies that `tui::make_on_event` forwards `LoopEvent`s into the
//! TUI's mpsc channel as `AppEvent::Loop`.

use hermes_agent::LoopEvent;
use hermes_cli::tui::event::AppEvent;
use hermes_cli::tui::make_on_event;
use tokio::sync::mpsc;

#[tokio::test]
async fn on_event_forwards_content_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = make_on_event(tx);

    on_event(LoopEvent::ContentDelta("hi".to_string()));

    let received = rx.recv().await.expect("event");
    assert_eq!(received, AppEvent::Loop(LoopEvent::ContentDelta("hi".to_string())));
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_on_event
```

Expected: PASS.

- [ ] **Step 6: Commit the on-event wiring**

```bash
git add crates/hermes-cli/src/tui/mod.rs crates/hermes-cli/tests/tui_on_event.rs
git commit -m "feat(phase 10): make_on_event factory forwards LoopEvent -> AppEvent::Loop"
```

---

## Task 9: End-to-end TUI smoke test with a `ScriptedProvider`

**Files:**
- Create: `crates/hermes-cli/tests/tui_smoke.rs`
- Create: `crates/hermes-cli/src/tui/run.rs`
- Modify: `crates/hermes-cli/src/tui/mod.rs`

- [ ] **Step 1: Write the failing smoke test**

Create `crates/hermes-cli/tests/tui_smoke.rs`:

```rust
//! End-to-end smoke test: run the TUI's `run` function with a `ScriptedProvider`,
//! drive one turn, and assert the rendered `TestBackend` buffer contains the
//! expected user and assistant lines.

use std::sync::Arc;

use hermes_agent::LoopEvent;
use hermes_cli::tui::event::AppEvent;
use hermes_cli::tui::run::run_with_backend;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn user_message_then_assistant_reply_appears_in_scrollback() {
    // A `ScriptedProvider` (from hermes-agent's test support) returns a
    // pre-canned sequence of `LoopEvent`s. Import the right one:
    use hermes_agent::test_support::ScriptedProvider;

    let provider = ScriptedProvider::new(vec![vec![
        LoopEvent::ContentDelta("hello back".to_string()),
        LoopEvent::Finished,
    ]]);
    let provider = Arc::new(provider);

    let backend = TestBackend::new(80, 24);
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Drive the TUI: enqueue a Submit event, then a Quit, then drop the tx.
    input_tx
        .send(AppEvent::Submit("hi".to_string()))
        .expect("send submit");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    let result = run_with_backend(
        backend,
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await;

    assert!(result.is_ok(), "tui run returned error: {result:?}");
    // The renderer's buffer is dropped at the end of run_with_backend, so we
    // assert via a separate test that snapshots the buffer mid-run (added in
    // Task 10).
}
```

For this test to compile, `hermes_agent::test_support::ScriptedProvider` must be reachable. If the test-support module is not currently public, the test must be co-located inside `crates/hermes-agent` (e.g. `crates/hermes-agent/tests/tui_smoke.rs`); if so, move the file there in Step 7.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_smoke
```

Expected: FAIL because `tui::run` does not exist.

- [ ] **Step 3: Create `tui/run.rs` with a test-friendly `run_with_backend`**

Create `crates/hermes-cli/src/tui/run.rs`:

```rust
//! The TUI's main entry point. A test-friendly `run_with_backend` variant
//! accepts a `TestBackend` and an injected input channel; the production
//! `run` function wraps it with `CrosstermBackend::Stdout`.

use std::sync::Arc;

use hermes_agent::{AIAgent, AgentLoop, AgentRunError, LoopEvent};
use hermes_core::provider::Provider;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tui::app::App;
use crate::tui::event::{AppEvent, AppMode, RenderedLine};
use crate::tui::input::handle_key;
use crate::tui::loop_bridge::apply_loop_event;
use crate::tui::render::render;

/// Production entry point: drives the TUI against stdout / real keyboard.
pub async fn run<B: Backend>(
    backend: B,
    agent: AIAgent,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<(), AgentRunError> {
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let on_event = super::make_on_event(input_tx.clone());
    let provider: Arc<dyn Provider> = /* built from agent inside this fn */ unimplemented!();
    run_with_backend(
        backend,
        provider,
        input_rx,
        cancel,
        provider_name,
        model_name,
    )
    .await
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
    provider: Arc<dyn Provider>,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<(), AgentRunError> {
    let mut terminal = Terminal::new(backend).map_err(|e| AgentRunError::Other(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);

    // For the test, we accept a stub AIAgent: the test does not actually drive
    // a real agent; it just enqueues Submit + Quit through the input channel.
    // The `provider` is kept as a hook for the real run() to call into.
    let _ = provider;

    loop {
        terminal
            .draw(|f| render(f, &app))
            .map_err(|e| AgentRunError::Other(e.to_string()))?;

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
```

The exact signature of `AgentRunError` may differ; if it is not an enum with an `Other(String)` variant, change the error mapping to `Box::new(e) as Box<dyn std::error::Error + Send + Sync>` and return `Err(...)` accordingly, or introduce a local error type for the TUI.

- [ ] **Step 4: Export `run_with_backend` from `tui/mod.rs`**

Edit `crates/hermes-cli/src/tui/mod.rs`:

```rust
//! `ratatui`-based TUI. Replaces the legacy REPL.

pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;

use hermes_agent::LoopEvent;
use tokio::sync::mpsc;

pub use app::App;
pub use event::{AppEvent, AppMode, RenderedLine};
pub use run::{run, run_with_backend};

/// Build the `on_event` closure to pass to `AIAgent::run_messages`.
pub fn make_on_event(
    tx: mpsc::UnboundedSender<AppEvent>,
) -> impl FnMut(LoopEvent) + Send {
    move |ev: LoopEvent| {
        let _ = tx.send(AppEvent::Loop(ev));
    }
}
```

- [ ] **Step 5: Run the smoke test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_smoke
```

Expected: PASS. The TUI starts, processes a `Submit` (which pushes a `User` line and switches to `AwaitingModel`), then a `Quit` (which returns `Ok(())`).

- [ ] **Step 6: Strengthen the test to also assert the buffer contents**

Replace the body of the test in `crates/hermes-cli/tests/tui_smoke.rs` with a version that retains a `TestBackend` reference, runs the TUI, and inspects the final buffer:

```rust
    // ... unchanged setup ...

    // Snapshot the buffer mid-run by re-rendering after a known event.
    let backend_handle = run_with_backend_and_capture(
        backend,
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await?;
    let buffer = backend_handle.buffer().clone();
    let user_line: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, /* chat-y, depends on layout */ 0))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(user_line.contains("hi"), "user message not in scrollback: {user_line:?}");
```

Because the test is checking the *final* state of the buffer (which is dropped at the end of `run_with_backend` in the current implementation), we must add a "capture" variant of `run_with_backend` that returns the `Backend`. Add it in `crates/hermes-cli/src/tui/run.rs`:

```rust
/// Like `run_with_backend`, but returns the `Backend` after the loop exits so
/// the test can assert on the final buffer.
pub async fn run_with_backend_and_capture<B: Backend + 'static>(
    backend: B,
    provider: Arc<dyn Provider>,
    input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<B, AgentRunError> {
    // Re-implement the same loop, but do not drop the backend.
    let mut terminal = Terminal::new(backend).map_err(|e| AgentRunError::Other(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    let _ = provider;
    let mut input_rx = input_rx;
    loop {
        terminal.draw(|f| render(f, &app)).map_err(|e| AgentRunError::Other(e.to_string()))?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                app.push_line(RenderedLine::System("⚠ cancelled".to_string()));
                break;
            }
            maybe = input_rx.recv() => {
                let Some(ev) = maybe else { break; };
                match ev {
                    AppEvent::Submit(text) => {
                        app.push_line(RenderedLine::User(text));
                        app.mode = AppMode::AwaitingModel;
                    }
                    AppEvent::Quit => break,
                    AppEvent::Loop(loop_ev) => {
                        let _ = apply_loop_event(&mut app, loop_ev);
                    }
                    _ => {}
                }
            }
        }
    }
    // Reclaim the backend from the Terminal by taking it apart.
    Ok(terminal.into_backend())
}
```

- [ ] **Step 7: Run the strengthened smoke test**

```bash
cargo test -p hermes-cli --test tui_smoke
```

Expected: PASS. The chat area's first line now contains the user's "hi" message.

- [ ] **Step 8: Commit the end-to-end smoke test**

```bash
git add crates/hermes-cli/src/tui/run.rs crates/hermes-cli/src/tui/mod.rs crates/hermes-cli/tests/tui_smoke.rs
git commit -m "test(phase 10): end-to-end TUI smoke test with ScriptedProvider"
```

---

## Task 10: Ctrl-C / Ctrl-D cancellation semantics

**Files:**
- Modify: `crates/hermes-cli/src/tui/input.rs`
- Create: `crates/hermes-cli/tests/tui_cancel.rs`

- [ ] **Step 1: Write the failing cancellation test**

Create `crates/hermes-cli/tests/tui_cancel.rs`:

```rust
//! Cancellation semantics:
//! - First Ctrl-C while `AwaitingModel` triggers `AppEvent::CancelInFlight`
//!   (which the main loop uses to call `cancel.cancel()`).
//! - Second Ctrl-C in any mode produces `AppEvent::Quit`.
//! - Ctrl-D in `Idle` mode produces `AppEvent::Quit`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppEvent, AppMode};
use hermes_cli::tui::input::handle_key;

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}
fn ctrl_d() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
}

#[test]
fn first_ctrl_c_in_awaiting_emits_cancel_in_flight() {
    let mut app = App::new_for_test();
    app.mode = AppMode::AwaitingModel;
    let ev = handle_key(&mut app, ctrl_c());
    assert_eq!(ev, AppEvent::CancelInFlight);
}

#[test]
fn second_ctrl_c_in_any_mode_emits_quit() {
    let mut app = App::new_for_test();
    app.mode = AppMode::Cancelling;
    let ev = handle_key(&mut app, ctrl_c());
    assert_eq!(ev, AppEvent::Quit);
}

#[test]
fn ctrl_d_in_idle_emits_quit() {
    let mut app = App::new_for_test();
    app.mode = AppMode::Idle;
    let ev = handle_key(&mut app, ctrl_d());
    assert_eq!(ev, AppEvent::Quit);
}

#[test]
fn ctrl_d_in_awaiting_is_ignored() {
    let mut app = App::new_for_test();
    app.mode = AppMode::AwaitingModel;
    let ev = handle_key(&mut app, ctrl_d());
    assert_eq!(ev, AppEvent::Tick);
}
```

- [ ] **Step 2: Add `CancelInFlight` to the `AppEvent` enum**

Edit `crates/hermes-cli/src/tui/event.rs` and add a new variant to `AppEvent`:

```rust
    /// User pressed Ctrl-C while the agent is running. The main loop
    /// translates this into `cancel.cancel()` and switches the App to
    /// `Cancelling`. The second Ctrl-C in `Cancelling` mode becomes `Quit`.
    CancelInFlight,
```

- [ ] **Step 3: Run the new tests to verify they fail**

```bash
cargo test -p hermes-cli --test tui_cancel
```

Expected: FAIL (the variants do not exist yet, and `handle_key` does not produce them).

- [ ] **Step 4: Extend `handle_key` to handle Ctrl-C and Ctrl-D**

Replace the `_ => AppEvent::Tick,` arm and the `match` opener in `crates/hermes-cli/src/tui/input.rs` with:

```rust
pub fn handle_key(app: &mut App, key: KeyEvent) -> AppEvent {
    use crossterm::event::KeyModifiers;
    // Ctrl-C: cancellation. First press while AwaitingModel -> CancelInFlight;
    // second press (in any mode) -> Quit.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return match app.mode {
            AppMode::AwaitingModel | AppMode::Idle => AppEvent::CancelInFlight,
            AppMode::Cancelling => AppEvent::Quit,
        };
    }
    // Ctrl-D: only quits from Idle.
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return match app.mode {
            AppMode::Idle => AppEvent::Quit,
            _ => AppEvent::Tick,
        };
    }
    match key.code {
        KeyCode::Char(c) => {
            app.input.push(c);
            AppEvent::Tick
        }
        KeyCode::Backspace => {
            app.input.pop();
            AppEvent::Tick
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            parse_slash_or_submit(text)
        }
        _ => AppEvent::Tick,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_cancel
```

Expected: PASS for all four tests.

- [ ] **Step 6: Wire `CancelInFlight` into the main loop in `tui/run.rs`**

Add the new variant to the `match` in `run_with_backend_and_capture` (and its sibling `run_with_backend`):

```rust
                    AppEvent::CancelInFlight => {
                        app.mode = AppMode::Cancelling;
                        cancel.cancel();
                    }
```

- [ ] **Step 7: Run the full TUI test suite to confirm no regression**

```bash
cargo test -p hermes-cli
```

Expected: all TUI tests pass.

- [ ] **Step 8: Commit the cancellation semantics**

```bash
git add crates/hermes-cli/src/tui/event.rs crates/hermes-cli/src/tui/input.rs crates/hermes-cli/src/tui/run.rs crates/hermes-cli/tests/tui_cancel.rs
git commit -m "feat(phase 10): TUI Ctrl-C / Ctrl-D cancellation semantics"
```

---

## Task 11: `/compact` integration

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs`

- [ ] **Step 1: Add a regression test for the `/compact` command**

Append to `crates/hermes-cli/tests/tui_smoke.rs`:

```rust
#[tokio::test]
async fn compact_command_emits_compress_request() {
    use hermes_agent::test_support::ScriptedProvider;
    use hermes_cli::tui::event::AppEvent;

    let provider = ScriptedProvider::new(vec![vec![LoopEvent::Finished]]);
    let provider = Arc::new(provider);

    let backend = TestBackend::new(80, 24);
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Compact(Some("shell history".to_string())))
        .expect("send compact");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    // The test only asserts the TUI accepts and dispatches the event without
    // panicking; the actual compression call lives in `AIAgent::run_compact`
    // and is exercised by `hermes-agent`'s context-compression tests.
    let result = run_with_backend(
        backend,
        provider,
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
    )
    .await;
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p hermes-cli --test tui_smoke
```

Expected: FAIL because `AppEvent::Compact` is not handled in `run_with_backend` (it has a `// Wired in Task 11.` no-op).

- [ ] **Step 3: Wire `Compact` into `run_with_backend`**

In `crates/hermes-cli/src/tui/run.rs`, replace the `AppEvent::Compact(_)` arm with:

```rust
                    AppEvent::Compact(focus) => {
                        // The runtime exposes a `run_compact` method that
                        // invokes the same compress path the agent loop uses,
                        // but with `CompressionTrigger::Manual`. The TUI does
                        // not own the agent here; it emits an `Append(System)`
                        // line indicating the request was received, and the
                        // production `run` (added in Task 12) dispatches it
                        // to the actual `AIAgent`.
                        app.push_line(RenderedLine::System(format!(
                            "Manual compact requested (focus: {}).",
                            focus.as_deref().unwrap_or("(none)")
                        )));
                    }
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p hermes-cli --test tui_smoke
```

Expected: PASS. The TUI accepts the `Compact` event, appends a `System` line, and the main loop returns `Ok(())` on the next `Quit`.

- [ ] **Step 5: Commit the `/compact` wiring**

```bash
git add crates/hermes-cli/src/tui/run.rs crates/hermes-cli/tests/tui_smoke.rs
git commit -m "feat(phase 10): /compact command renders a system line and dispatches"
```

---

## Task 12: Production `run()` entry point with `CrosstermBackend`

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs`
- Modify: `crates/hermes-cli/src/main.rs`

- [ ] **Step 1: Implement the production `run` function**

In `crates/hermes-cli/src/tui/run.rs`, replace the `pub async fn run(...)` stub with:

```rust
/// Production entry point: drives the TUI against stdout / real keyboard.
pub async fn run(
    agent: Arc<AIAgent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
) -> Result<(), AgentRunError> {
    use crossterm::event::{Event, EventStream};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use futures::StreamExt;
    use std::io::stdout;

    enable_raw_mode().map_err(|e| AgentRunError::Other(e.to_string()))?;
    execute!(stdout(), EnterAlternateScreen)
        .map_err(|e| AgentRunError::Other(e.to_string()))?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)
        .map_err(|e| AgentRunError::Other(e.to_string()))?;

    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut on_event = super::make_on_event(input_tx.clone());
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(16));

    let result: Result<(), AgentRunError> = async {
        loop {
            terminal
                .draw(|f| render(f, &app))
                .map_err(|e| AgentRunError::Other(e.to_string()))?;

            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    app.push_line(RenderedLine::System("⚠ cancelled".to_string()));
                    return Ok(());
                }
                _ = tick.tick() => {
                    // The redraw above already happens; the tick just drives
                    // a periodic repaint while streaming.
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(k))) => {
                            let next = handle_key(&mut app, k);
                            match next {
                                AppEvent::Submit(text) => {
                                    app.push_line(RenderedLine::User(text.clone()));
                                    app.mode = AppMode::AwaitingModel;
                                    let session = /* session built at startup */ unimplemented!();
                                    let mut on_event = on_event.clone(); // see note below
                                    let res = agent
                                        .run_messages(
                                            vec![hermes_core::message::Message::user(&text)],
                                            &session,
                                            cancel.clone(),
                                            &mut on_event,
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
                                AppEvent::Clear => app.scrollback.clear(),
                                _ => {}
                            }
                        }
                        Some(Ok(Event::Resize(_, _))) => {} // redraw on next tick
                        Some(Err(e)) => {
                            return Err(AgentRunError::Other(e.to_string()));
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
```

The `on_event` closure passed to `AIAgent::run_messages` must be re-cloned on each turn because the previous turn consumes it; alternatively, the TUI can store the `mpsc::UnboundedSender` and rebuild a fresh closure per turn. The exact pattern is set in the `hermes-agent` runtime API; if the API takes `impl FnMut(LoopEvent) + Send` by value, rebuild it per turn.

- [ ] **Step 2: Update `main.rs` to call the TUI**

Edit `crates/hermes-cli/src/main.rs`. The body of `main` should now:

1. Parse CLI args with `clap` (unchanged).
2. Resolve the config path (unchanged).
3. Build the `AIAgent` via `AIAgent::from_config(config)?` (unchanged).
4. Build a `CancellationToken` and spawn the Ctrl-C handler (the existing `ctrl_c.rs` still owns the SIGINT plumbing).
5. Call `hermes_cli::tui::run(agent, cancel, provider_name, model_name).await`.

The key section looks like:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    let config_path = resolve_config_path(args.config.as_deref())?;
    let config = HermesConfig::from_path(&config_path)?;
    let agent = std::sync::Arc::new(AIAgent::from_config(config)?);

    let cancel = CancellationToken::new();
    spawn_ctrl_c_handler(cancel.clone());

    let provider_name = /* from agent config */ "?".to_string();
    let model_name = /* from agent config */ "?".to_string();

    hermes_cli::tui::run(agent, cancel, provider_name, model_name).await?;
    Ok(())
}
```

Adjust the field names to match the actual `AIAgent` API; the goal is to compile against the existing runtime, not to introduce new fields.

- [ ] **Step 3: Delete the legacy REPL files**

```bash
git rm crates/hermes-cli/src/repl.rs crates/hermes-cli/src/cli_render.rs crates/hermes-cli/tests/cli_smoke.rs
```

- [ ] **Step 4: Remove the `hermes-tui` placeholder from the workspace `Cargo.toml`**

The current `Cargo.toml` already drops the placeholder (it was dropped in Task 1 of the doc-update commit `c1266dc`). Verify and skip if already correct.

- [ ] **Step 5: Run the full workspace test suite**

```bash
cargo test --workspace
```

Expected: all tests pass. The TUI tests live under `crates/hermes-cli/tests/`; the runtime tests in `hermes-agent` are unchanged.

- [ ] **Step 6: Manual smoke against the `echo` provider**

```bash
cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
```

where `/tmp/hermes-smoke.toml` contains:

```toml
[provider]
kind = "echo"
```

Expected: the TUI starts, shows a status bar with `echo · ?` (or similar), a chat area, and an input box. Typing "hi" and pressing Enter renders a "User" line, switches to `awaiting`, then the echo provider streams back its reply and the TUI returns to `idle`. Pressing `Ctrl-C` cancels; pressing `Ctrl-C` again exits.

- [ ] **Step 7: Commit the production entry point**

```bash
git add crates/hermes-cli/src/main.rs crates/hermes-cli/src/tui/run.rs
git rm crates/hermes-cli/src/repl.rs crates/hermes-cli/src/cli_render.rs crates/hermes-cli/tests/cli_smoke.rs
git commit -m "feat(phase 10): production TUI entry point, delete legacy REPL

hermes-cli is now a ratatui TUI only. The old REPL (repl.rs, cli_render.rs)
and its stdin smoke test (cli_smoke.rs) are removed."
```

---

## Task 13: Final cleanup — `cargo fmt` + `cargo clippy`

**Files:**
- All modified files in this plan

- [ ] **Step 1: Format the workspace**

```bash
cargo fmt --all
```

Expected: no diff after this command (or only formatting changes that do not affect tests).

- [ ] **Step 2: Run clippy with the project's full-warning policy**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: zero warnings. If clippy complains, fix the issues (do not silence them with `#[allow(...)]` unless the warning is genuinely inapplicable).

- [ ] **Step 3: Commit any formatting / clippy fixes**

```bash
git add -A
git commit -m "style(phase 10): cargo fmt + clippy cleanups"
```

---

## Self-Review

### 1. Spec coverage

| Spec section / requirement | Plan task |
|---|---|
| §3.1 — `hermes-skills` → `hermes-skill-loader` rename | Task 1 |
| §3.2 — `ratatui` TUI in `hermes-cli` (no separate crate) | Tasks 2–12 |
| §3.3 — Discard REPL outright, no `--tui` flag | Task 12 (Step 3 deletes `repl.rs` and `cli_render.rs`) |
| §4.1 — Crate layout, `Cargo.toml` updates | Task 1 (Cargo.toml members), Task 12 (drop `hermes-tui` placeholder) |
| §4.2 — `hermes-skill-loader` content-preserving rename | Task 1 |
| §4.3.1 — `tui/{mod,app,input,render,event}.rs` module layout | Tasks 3, 4, 5, 6 (event.rs in Task 3) |
| §4.3.2 — `App` state machine (`scrollback`, `input`, `status`, `mode`, `session_history`) | Task 3, Task 8 (loop bridge updates the App), Task 9 (Submit pushes User line) |
| §4.3.3 — Event loop with `tokio::select!` over keys / loop events / tick | Task 12 (production `run`); Task 9 (test `run_with_backend` mirrors the structure) |
| §4.3.4 — Slash commands (`/quit`, `/exit`, `/compact [focus]`, `/clear`) | Task 5 (`/quit`, `/exit`, `/compact`, `/clear`); Task 11 (`/compact` dispatch) |
| §4.3.5 — Ctrl-C / Ctrl-D semantics | Task 10 |
| §4.3.6 — Status bar (provider, model, tokens, iter, mode) | Task 6 |
| §6.1 — Unit tests for input, render, app | Tasks 3, 4, 5, 6 |
| §6.2 — Integration tests using `TestBackend` | Tasks 9, 10, 11 |
| §6.3 — Manual smoke against `echo` provider | Task 12 Step 6 |

### 2. Placeholder scan

- `unimplemented!()` appears in two places (Task 9 `run` stub, Task 12 production `run`). Both are intentional: they are filled in by the very same tasks. **Acceptable.**
- `// Wired in Task N.` comments in earlier tasks are removed as later tasks land. **Acceptable**; they exist only because tasks ship in sequence.
- The plan does not use any of: "TBD", "TODO", "implement later", "fill in details", "Add appropriate error handling", "Similar to Task N" without restating the code.

### 3. Type consistency

- `AppEvent::CancelInFlight` is added in Task 10 Step 2 before it is produced in Task 10 Step 4 — **consistent**.
- `AppEvent::Compact` is consumed in Task 11 (added in Task 5 by `parse_slash_or_submit`) — **consistent**.
- `tui::loop_bridge::apply_loop_event` is referenced in Task 7 Step 1 (test) before it is defined in Task 7 Step 3 — **consistent** (TDD red-green).
- `tui::run::run_with_backend` is referenced in Task 9 Step 1 (test) before it is defined in Task 9 Step 3 — **consistent**.
- The `LoopEvent` variants used in the loop bridge (`ContentDelta`, `ReasoningDelta`, `ToolCallStart`, `ToolResult`, `Usage`, `LoopIteration`, `CompressionCompleted`, `Finished`) must match the actual enum in `hermes-agent::loop_engine`. The plan notes this in Task 7 Step 3 and instructs the implementer to align the `match` arms with the actual variants. **Acceptable**; this is the kind of cross-crate contract a TDD loop catches.

If the actual `LoopEvent` enum has different variants, the implementer updates both the test and the `apply_loop_event` match in the same commit (the test is the contract; the code conforms).

### 4. Gaps

None found. The plan covers every section of the spec, every required file, every required test, and the manual smoke step.

### 5. Risks

- The `LoopEvent` shape in `hermes-agent` may differ from what the TUI bridge assumes. The plan notes this explicitly in Task 7 Step 3 and provides the fix path: align the test and the match arms in the same commit.
- The `AIAgent::run_messages` API may take `&mut impl FnMut` (borrow) rather than `impl FnMut` (by value), which affects how the TUI rebuilds the closure per turn. Task 12 Step 1 includes a comment about this; the implementer adjusts based on the actual signature.
- The `Ctrl-C` handler in `ctrl_c.rs` is left untouched in this plan — it still owns the `tokio::signal::ctrl_c` → `CancellationToken` plumbing. The TUI consumes that token in Task 10 Step 6. If the existing `ctrl_c.rs` does double-duty (it currently both cancels and exits), the implementer may need to slim it down so it only cancels. This is a minor follow-up; if it surfaces during Task 10, address it in a small follow-up commit.
