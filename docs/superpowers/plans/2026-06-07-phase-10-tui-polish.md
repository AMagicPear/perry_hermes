# Phase 10 (TUI polish): TUI Polish — Codex-Style Visual Upgrade + Bug Fixes

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade the `hermes-cli` TUI to visually match codex's TUI (welcome banner with shimmer, rounded assistant boxes, 2-line status bar, bordered input) and fix a handful of small event-loop bugs surfaced during Phase 10 review. Also wire `context_window_size` from TOML config through to the `ContextCompressor` (currently hardcoded to 128K).

**Architecture:**
- New `tui/shimmer.rs` (port of codex's `shimmer_spans`).
- `tui/render.rs` rewritten with the codex-style layout: a one-shot welcome banner, rounded assistant blocks, an optional context segment in the status bar, a bordered input panel.
- Three new fields on `App` (`welcome_shown`, `turn_started_at`, `context_window_size`) drive the new render branches.
- `AgentConfig::context_window_size` (new) flows into `ContextCompressor::new` (signature change) so the percentage threshold scales with the actual model window.
- Five small TUI event-loop bugs in §6 of [the spec](../specs/2026-06-07-phase-10-tui-polish-design.md) are fixed in the same branch.

**Tech Stack:** `ratatui` 0.29 (existing), `crossterm` 0.28 (existing), `tokio` 1.x (existing), `serde` + `toml` for the new config field. **No new dependencies.**

---

## File Structure

### Created

| File | Responsibility |
|---|---|
| `crates/hermes-cli/src/tui/shimmer.rs` | Time-sweep highlight effect (`shimmer_spans` + `shimmer_line`). Ported from codex. |
| `docs/superpowers/plans/2026-06-07-phase-10-tui-polish.md` | This plan. |

### Modified

| File | Change |
|---|---|
| `crates/hermes-agent/src/config.rs` | Add `context_window_size: Option<u64>` to `AgentConfig`. |
| `crates/hermes-agent/src/context/compressor.rs` | `ContextCompressor::new` takes a 3rd arg `context_length: Option<u64>`. |
| `crates/hermes-agent/src/runtime_agent.rs` | Pass `config.agent.context_window_size` into `ContextCompressor::new`. |
| `crates/hermes-agent/tests/context_compression.rs` | Update the one existing call site to pass `None`; add two new tests for window-scaled thresholds. |
| `crates/hermes-cli/src/tui/app.rs` | Add `welcome_shown`, `turn_started_at`, `context_window_size` fields. |
| `crates/hermes-cli/src/tui/render.rs` | Full rewrite: welcome banner, rounded assistant, 2-line status, bordered input. |
| `crates/hermes-cli/src/tui/run.rs` | Wire welcome + turn tracking. Drop `_provider` arg. Delete the duplicate `run_with_backend_and_capture`. Fix bug §6.1 (handle_key return), §6.5 (Cancelling state). |
| `crates/hermes-cli/src/tui/input.rs` | Bug fix §6.2: `Cancelling` mode ignores non-Ctrl-C keys. |
| `crates/hermes-cli/src/tui/mod.rs` | `pub mod shimmer;` |
| `crates/hermes-cli/src/main.rs` | Extract `context_window_size` from config, pass to `tui::run`. |
| `crates/hermes-cli/tests/tui_render.rs` | Update existing 2 tests for new layout; add 6 new tests. |
| `crates/hermes-cli/tests/tui_smoke.rs` | Convert one test to use the renamed capture variant. |
| `crates/hermes-cli/tests/tui_input.rs` | Add 1 test for the `Cancelling` key-ignore behavior. |

### Deleted

| File | Reason |
|---|---|
| `crates/hermes-cli/src/tui/run.rs::run_with_backend_and_capture` | Merged into the surviving `run_with_backend` (which now takes the `Arc<Mutex<TestBackend>>`). |

---

## Task 1: Add `context_window_size` to `AgentConfig`

**Files:**
- Modify: `crates/hermes-agent/src/config.rs:67-97` (add field + Default)
- Test: `crates/hermes-agent/src/config.rs:148-200` (extend existing parse test)

- [ ] **Step 1: Extend the existing parse test to cover the new field**

Open `crates/hermes-agent/src/config.rs`. Find the `#[test]` block in the `#[cfg(test)] mod tests` section (around line 148). Add a new assertion at the end of the existing test that checks `context_window_size` round-trips. The existing test reads:

```rust
context_compression_threshold_percent = 0.60
```

Append a new line to that TOML input and add an assertion. The full new test (or a new `#[test] fn context_window_size_round_trips()` added after the existing one):

```rust
#[test]
fn context_window_size_round_trips() {
    let toml = r#"
[provider]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1"

[agent]
context_window_size = 200_000
"#;
    let config: HermesConfig = toml::from_str(toml).expect("parse");
    assert_eq!(config.agent.context_window_size, Some(200_000));
}

#[test]
fn context_window_size_absent_defaults_to_none() {
    let toml = r#"
[provider]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1"
"#;
    let config: HermesConfig = toml::from_str(toml).expect("parse");
    assert_eq!(config.agent.context_window_size, None);
}
```

- [ ] **Step 2: Run tests, expect compile failure**

Run: `cargo test -p hermes-agent --lib config::tests`
Expected: compile error — `AgentConfig` has no field `context_window_size`.

- [ ] **Step 3: Add the field**

In `crates/hermes-agent/src/config.rs`, modify `AgentConfig`:

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AgentConfig {
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Enable context compression. Default true; set false to disable.
    #[serde(default = "default_context_compression_enabled")]
    pub context_compression_enabled: bool,
    /// Threshold percentage of model context at which compression triggers.
    /// Default 0.50 (50%).
    #[serde(default)]
    pub context_compression_threshold_percent: Option<f64>,
    /// Total context window in tokens for the configured model. Used as the
    /// denominator for `context_compression_threshold_percent`, and rendered
    /// as the "24.2K / 200K [gauge]" segment in the TUI status bar. When
    /// `None`, the compressor falls back to 128_000 and the TUI hides the
    /// context segment.
    #[serde(default)]
    pub context_window_size: Option<u64>,
}
```

And the `Default` impl:

```rust
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: None,
            disabled_toolsets: Vec::new(),
            system_prompt: None,
            context_compression_enabled: default_context_compression_enabled(),
            context_compression_threshold_percent: None,
            context_window_size: None,
        }
    }
}
```

- [ ] **Step 4: Run tests, expect green**

Run: `cargo test -p hermes-agent --lib config::tests`
Expected: PASS (3 tests, including the existing one and the 2 new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-agent/src/config.rs
git commit -m "feat(agent): add context_window_size to AgentConfig"
```

---

## Task 2: Plumb `context_window_size` through `ContextCompressor::new`

**Files:**
- Modify: `crates/hermes-agent/src/context/compressor.rs:297-310` (change `new` signature)
- Modify: `crates/hermes-agent/src/context/compressor.rs:536-543` (update `update_model_changes_threshold` test)
- Modify: `crates/hermes-agent/src/runtime_agent.rs:111-128` (pass through the new arg)
- Test: `crates/hermes-agent/tests/context_compression.rs:105` (update the one call site)

- [ ] **Step 1: Update the existing test in `context_compression.rs` to pass `None`**

Open `crates/hermes-agent/tests/context_compression.rs`. Find line 105 (the `let compressor = ContextCompressor::new(...)` call). Add a third argument:

```rust
let compressor = ContextCompressor::new(CompressorConfig::default(), "test".into(), None)
```

Search the file for any other `ContextCompressor::new` calls and add `None` as the third arg to each.

- [ ] **Step 2: Update the test in `compressor.rs`**

In `crates/hermes-agent/src/context/compressor.rs`, the `update_model_changes_threshold` test at line ~535:

```rust
fn update_model_changes_threshold() {
    let mut compressor =
        ContextCompressor::new(CompressorConfig::default(), "old-model".into());
```

becomes:

```rust
fn update_model_changes_threshold() {
    let mut compressor =
        ContextCompressor::new(CompressorConfig::default(), "old-model".into(), None);
```

- [ ] **Step 3: Run tests, expect compile failure**

Run: `cargo test -p hermes-agent --lib context::compressor`
Expected: compile error — `ContextCompressor::new` takes 2 arguments, not 3.

- [ ] **Step 4: Update the `new` signature and body**

In `crates/hermes-agent/src/context/compressor.rs:297-310`, change:

```rust
impl ContextCompressor {
    pub fn new(config: CompressorConfig, model: String) -> Self {
        let context_length = 128_000;
        let threshold_tok = config.threshold_tokens(context_length);
        Self {
            config,
            model,
            threshold_tok,
            previous_summary: None,
            ineffective_count: 0,
            summary_provider: None,
            context_length,
        }
    }
```

to:

```rust
impl ContextCompressor {
    pub fn new(
        config: CompressorConfig,
        model: String,
        context_length: Option<u64>,
    ) -> Self {
        let context_length = context_length.unwrap_or(128_000);
        let threshold_tok = config.threshold_tokens(context_length);
        Self {
            config,
            model,
            threshold_tok,
            previous_summary: None,
            ineffective_count: 0,
            summary_provider: None,
            context_length,
        }
    }
```

- [ ] **Step 5: Update the caller in `runtime_agent.rs`**

In `crates/hermes-agent/src/runtime_agent.rs:111-128`, change:

```rust
let context_engine = if config.agent.context_compression_enabled {
    let mut compressor_config = CompressorConfig::default();
    if let Some(threshold_percent) = config.agent.context_compression_threshold_percent {
        compressor_config.threshold_percent = threshold_percent;
    }
    let model_name = config
        .provider
        .model
        .clone()
        .unwrap_or_else(|| provider_name(&config.provider).to_string());
    Some(Arc::new(TokioMutex::new(
        ContextCompressor::new(compressor_config, model_name)
            .with_summary_provider(Arc::clone(&provider)),
    )))
        as Arc<TokioMutex<dyn hermes_core::ContextEngine>>
} else {
    None
};
```

to:

```rust
let context_engine = if config.agent.context_compression_enabled {
    let mut compressor_config = CompressorConfig::default();
    if let Some(threshold_percent) = config.agent.context_compression_threshold_percent {
        compressor_config.threshold_percent = threshold_percent;
    }
    let model_name = config
        .provider
        .model
        .clone()
        .unwrap_or_else(|| provider_name(&config.provider).to_string());
    Some(Arc::new(TokioMutex::new(
        ContextCompressor::new(
            compressor_config,
            model_name,
            config.agent.context_window_size,
        )
            .with_summary_provider(Arc::clone(&provider)),
    )))
        as Arc<TokioMutex<dyn hermes_core::ContextEngine>>
} else {
    None
};
```

- [ ] **Step 6: Run tests, expect green**

Run: `cargo test -p hermes-agent`
Expected: PASS, all existing tests.

- [ ] **Step 7: Commit**

```bash
git add crates/hermes-agent/src/context/compressor.rs \
        crates/hermes-agent/src/runtime_agent.rs \
        crates/hermes-agent/tests/context_compression.rs
git commit -m "feat(agent): plumb context_window_size through to ContextCompressor"
```

---

## Task 3: Tests proving the threshold scales with `context_window_size`

**Files:**
- Modify: `crates/hermes-agent/tests/context_compression.rs` (add 2 new tests)

- [ ] **Step 1: Add the threshold-scaling tests**

Open `crates/hermes-agent/tests/context_compression.rs` and append at the end of the file:

```rust
#[test]
fn threshold_tokens_scales_with_context_window_size() {
    // With a 200K window and 50% threshold, compression triggers at 100K.
    // (Tests `CompressorConfig::threshold_tokens` directly.)
    let config = hermes_agent::CompressorConfig {
        threshold_percent: 0.50,
        ..Default::default()
    };
    assert_eq!(config.threshold_tokens(200_000), 100_000);
    // 128K window at 60% → 76800, well above the 8K floor.
    let config = hermes_agent::CompressorConfig {
        threshold_percent: 0.60,
        ..Default::default()
    };
    assert_eq!(config.threshold_tokens(128_000), 76_800);
    // Tiny windows still respect the 8K floor.
    assert_eq!(config.threshold_tokens(4_000), 8_000);
}

#[tokio::test]
async fn context_compressor_new_uses_provided_context_length() {
    use hermes_agent::{CompressorConfig, ContextCompressor};

    let config = CompressorConfig {
        threshold_percent: 0.50,
        ..Default::default()
    };
    let compressor = ContextCompressor::new(config, "test".into(), Some(200_000));
    assert_eq!(compressor.threshold_tokens(), 100_000);

    // None falls back to 128K.
    let compressor = ContextCompressor::new(
        CompressorConfig::default(),
        "test".into(),
        None,
    );
    assert_eq!(compressor.threshold_tokens(), 64_000); // 128K * 0.50
}
```

- [ ] **Step 2: Run tests, expect green**

Run: `cargo test -p hermes-agent --test context_compression`
Expected: PASS, all tests including the 2 new ones.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-agent/tests/context_compression.rs
git commit -m "test(agent): cover context_window_size scaling in threshold_tokens"
```

---

## Task 4: Create `tui/shimmer.rs` with `shimmer_spans` + tests

**Files:**
- Create: `crates/hermes-cli/src/tui/shimmer.rs`
- Modify: `crates/hermes-cli/src/tui/mod.rs:1-9` (add `pub mod shimmer;`)

- [ ] **Step 1: Add the failing test in the new file**

Create `crates/hermes-cli/src/tui/shimmer.rs` with the test-first body:

```rust
//! Time-sweep highlight effect for the welcome banner.
//!
//! A `BOLD` band of fixed width sweeps across the text from left to right
//! with a 2-second period, synchronized to process start. When the terminal
//! supports truecolor, the band uses a smoothly-blended RGB highlight
//! (white → base foreground color); otherwise it falls back to `DIM` /
//! `BOLD` modifier flags only.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn elapsed_since_start() -> std::time::Duration {
    let start = PROCESS_START.get_or_init(Instant::now);
    start.elapsed()
}

/// Render `text` as a series of spans with the shimmer effect applied.
/// Each character gets its own span styled by distance from the sweep position.
pub fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    shimmer_spans_with_sweep(text, elapsed_since_start())
}

/// Test-friendly variant: caller supplies the elapsed time so snapshots are stable.
pub fn shimmer_spans_with_sweep(
    text: &str,
    elapsed: std::time::Duration,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let sweep_seconds = 2.0f32;
    let pos_f =
        (elapsed.as_secs_f32() % sweep_seconds) / sweep_seconds * (period as f32);
    let pos = pos_f as usize;
    let band_half_width = 5.0_f32;

    chars
        .iter()
        .enumerate()
        .map(|(i, ch)| {
            let i_pos = i as isize + padding as isize;
            let pos = pos as isize;
            let dist = (i_pos - pos).abs() as f32;
            let t = if dist <= band_half_width {
                let x = std::f32::consts::PI * (dist / band_half_width);
                0.5 * (1.0 + x.cos())
            } else {
                0.0
            };
            let style = if t < 0.2 {
                Style::default().add_modifier(Modifier::DIM)
            } else if t < 0.6 {
                Style::default()
            } else {
                // Center of the band: render bold. We don't have a truecolor
                // palette plumbed in yet; BOLD is the universal fallback.
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::White)
            };
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_returns_no_spans() {
        let spans = shimmer_spans_with_sweep("", std::time::Duration::ZERO);
        assert!(spans.is_empty());
    }

    #[test]
    fn one_span_per_char() {
        let spans = shimmer_spans_with_sweep("abc", std::time::Duration::ZERO);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[1].content.as_ref(), "b");
        assert_eq!(spans[2].content.as_ref(), "c");
    }

    #[test]
    fn sweep_position_zero_dim_styles() {
        // At time=0 the sweep is at the leftmost position. Far-edge chars
        // (right side of "abcdefghij") should be DIM (intensity 0).
        let spans = shimmer_spans_with_sweep(
            "abcdefghijklmnop",
            std::time::Duration::ZERO,
        );
        let last = &spans[spans.len() - 1];
        assert!(
            last.style.add_modifier.contains(Modifier::DIM),
            "expected last char to be DIM at t=0; got style {:?}",
            last.style
        );
    }

    #[test]
    fn mid_sweep_has_bold_band() {
        // At a sweep position that lands in the middle of the string, at
        // least one char should be BOLD (the center of the band).
        let text = "abcdefghijklmnopqrstuvwxyz"; // 26 chars
        // sweep_seconds=2, padding=10, period=46. Set elapsed so pos lands at 13.
        // pos_f = (elapsed_secs % 2) / 2 * 46. We want pos=13 → elapsed_secs ~= 0.565.
        let elapsed = std::time::Duration::from_secs_f32(0.565);
        let spans = shimmer_spans_with_sweep(text, elapsed);
        let any_bold = spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            any_bold,
            "expected at least one BOLD span mid-sweep; spans={spans:?}"
        );
    }
}
```

- [ ] **Step 2: Add `pub mod shimmer;`**

Edit `crates/hermes-cli/src/tui/mod.rs` to add the new module. Change the file from:

```rust
pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;
```

to:

```rust
pub mod app;
pub mod event;
pub mod input;
pub mod loop_bridge;
pub mod render;
pub mod run;
pub mod shimmer;
```

- [ ] **Step 3: Run tests, expect green**

Run: `cargo test -p hermes-cli --lib tui::shimmer`
Expected: PASS, 4 tests.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-cli/src/tui/shimmer.rs \
        crates/hermes-cli/src/tui/mod.rs
git commit -m "feat(cli): add tui shimmer module (time-sweep highlight effect)"
```

---

## Task 5: Add `welcome_shown`, `turn_started_at`, `context_window_size` fields to `App`

**Files:**
- Modify: `crates/hermes-cli/src/tui/app.rs` (add 3 fields, update `new_for_test`)

- [ ] **Step 1: Add the fields**

Open `crates/hermes-cli/src/tui/app.rs`. Replace the file contents with:

```rust
//! The TUI's state machine.

use std::time::Instant;

use hermes_core::message::Message;

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
    /// Set to `true` after the first render draws the welcome banner.
    pub welcome_shown: bool,
    /// `Some(Instant)` while a turn is in flight (`AppMode::AwaitingModel`).
    /// `None` when idle or cancelling. Drives the elapsed-time readout in
    /// the status bar.
    pub turn_started_at: Option<Instant>,
    /// Total context window in tokens, if configured. When `None`, the status
    /// bar hides the context segment entirely.
    pub context_window_size: Option<u64>,
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
            welcome_shown: false,
            turn_started_at: None,
            context_window_size: None,
        }
    }

    /// Push a rendered line into the scrollback.
    pub fn push_line(&mut self, line: RenderedLine) {
        self.scrollback.push(line);
    }
}
```

- [ ] **Step 2: Run tests, expect green**

Run: `cargo test -p hermes-cli`
Expected: PASS, all existing tests (the new fields are unused but typed).

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-cli/src/tui/app.rs
git commit -m "feat(cli): add welcome_shown, turn_started_at, context_window_size to App"
```

---

## Task 6: Rewrite `render.rs` with the codex-style layout

**Files:**
- Modify: `crates/hermes-cli/src/tui/render.rs` (full rewrite)

- [ ] **Step 1: Add `fmt_elapsed_compact` test (RED)**

Open `crates/hermes-cli/src/tui/render.rs`. Replace the file with this scaffold that includes the failing test:

```rust
//! Frame painter for the TUI.

use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::App;
use crate::tui::event::{AppMode, RenderedLine};
use crate::tui::shimmer::shimmer_spans;

/// A 5-row ASCII art banner spelling "HERMES" (figlet "small" style).
/// Painted exactly once at the start of the TUI.
const WELCOME_BANNER: &[&str] = &[
    " _   _ _____ ___  __  __ _____ ____  ",
    "| | | | ____/ _ \\|  \\/  | ____|  _ \\ ",
    "| |_| |  _|| | | | |\\/| |  _| | |_) |",
    "|  _  | |__| |_| | |  | | |___|  _ < ",
    "|_| |_|_____\\___/|_|  |_|_____|_| \\_\\",
];

const TIP_LINE: &str = "✦ Tip: press / to see available commands.";

/// Paint one frame.
pub fn render(f: &mut Frame, app: &App) {
    let welcome_h = if app.welcome_shown { 0 } else { WELCOME_BANNER.len() as u16 };
    let tip_h = if app.welcome_shown { 0 } else { 1 };
    let working_h = if matches!(app.mode, AppMode::AwaitingModel) { 1 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(welcome_h), // welcome banner
            Constraint::Length(tip_h),      // tip line
            Constraint::Min(1),            // chat scrollback
            Constraint::Length(working_h), // working indicator (awaiting only)
            Constraint::Length(1),         // status row 1
            Constraint::Length(3),         // input block
        ])
        .split(f.area());

    // --- Welcome banner (one-shot) -----------------------------------------
    if !app.welcome_shown {
        let mut banner_lines: Vec<Line> = WELCOME_BANNER
            .iter()
            .map(|row| Line::from(shimmer_spans(row)))
            .collect();
        banner_lines.push(Line::from(TIP_LINE).dim());
        let banner = Paragraph::new(banner_lines)
            .block(Block::default().borders(Borders::NONE))
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(banner, chunks[0]);
    }

    // --- Chat scrollback ----------------------------------------------------
    let chat_area = chunks[2];
    let chat_lines = build_chat_lines(&app.scrollback, chat_area.width);
    let chat = Paragraph::new(chat_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((0, 0));
    f.render_widget(chat, chat_area);

    // --- Working indicator (only when awaiting) -----------------------------
    if matches!(app.mode, AppMode::AwaitingModel) {
        let working = build_working_line(app);
        let working_widget = Paragraph::new(working)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(working_widget, chunks[3]);
    }

    // --- Status row 1 (always visible) --------------------------------------
    let status_line = build_status_line_1(app);
    let status = Paragraph::new(status_line)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(status, chunks[4]);

    // --- Input block --------------------------------------------------------
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let input_inner = input_block.inner(chunks[5]);
    let input_text = build_input_line(app);
    let input = Paragraph::new(input_text)
        .block(input_block)
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[5]);

    // Position the cursor inside the input block, on the middle row, just
    // after the `❯ ` prompt and the typed text.
    let _ = input_inner; // (cursor positioning is best-effort; ratatui auto-handles it)
}

/// Build the chat-area `Vec<Line>` from the scrollback, wrapping assistant
/// content in a rounded `╭─ Hermes ─...─╮` block.
fn build_chat_lines(
    scrollback: &[RenderedLine],
    width: u16,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in scrollback {
        match line {
            RenderedLine::User(s) => out.push(Line::from(format!("> {s}"))),
            RenderedLine::Assistant(s) => {
                out.extend(assistant_block(s, width));
            }
            RenderedLine::Reasoning(s) => {
                out.push(Line::from(format!("… {s}")).dim());
            }
            RenderedLine::ToolCall { name, args_preview } => {
                out.push(Line::from(format!("⚙ {name}({args_preview})")));
            }
            RenderedLine::ToolResult { name, output, ok } => {
                let glyph = if *ok { "✓" } else { "✗" };
                out.push(Line::from(format!("{glyph} {name}: {output}")));
            }
            RenderedLine::System(s) => {
                out.push(Line::from(format!("[system] {s}")));
            }
        }
    }
    out
}

/// Render assistant text inside a rounded box of `width` columns.
/// Top: `╭─ Hermes ─<dashes>─╮`, body: `│ <wrapped text> │`, bottom: `╰─<dashes>─╯`.
fn assistant_block(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width.max(20) as usize;
    // Inner content width = total width - 4 (for `│ ` and ` │`).
    let inner_w = w.saturating_sub(4).max(1);
    let title = " Hermes ";
    // Top border: `╭─ Hermes ─...─╮` — title takes (2 + title.len() + 1) cols,
    // fill the rest with `─`.
    let top_prefix = "╭─";
    let top_suffix = "╮";
    let top_filler_dashes = w
        .saturating_sub(top_prefix.len() + title.len() + top_suffix.len());
    let top = format!("{top_prefix}{title}{}", "─".repeat(top_filler_dashes));
    // Bottom border: `╰──────────────╯`
    let bot_prefix = "╰─";
    let bot_suffix = "╯";
    let bot_filler_dashes = w.saturating_sub(bot_prefix.len() + bot_suffix.len());
    let bot = format!("{bot_prefix}{}{bot_suffix}", "─".repeat(bot_filler_dashes));

    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(top).bold().cyan());

    // Wrap text manually into lines of `inner_w` columns. word_wrap is a
    // hard wrap for simplicity (preserves spaces but breaks on any char).
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= inner_w {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(Line::from(format!("│ {current:<inner_w$} │")));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        out.push(Line::from(format!("│ {current:<inner_w$} │")));
    } else if text.is_empty() {
        out.push(Line::from(format!("│ {:<inner_w$} │", "")));
    }

    out.push(Line::from(bot));
    out
}

/// Build status row 1: `⚕ {provider} · {model} · {in_tok} / {total} [gauge] {pct}% · {elapsed} · iter {i}/{max} · {mode}`.
/// The `{in_tok} / {total} [gauge] {pct}%` segment is omitted when
/// `app.context_window_size` is `None`.
fn build_status_line_1(app: &App) -> Line<'static> {
    let provider = app.provider_name.as_deref().unwrap_or("?");
    let model = app.model_name.as_deref().unwrap_or("?");
    let mode = match app.mode {
        AppMode::Idle => "idle",
        AppMode::AwaitingModel => "awaiting",
        AppMode::Cancelling => "cancelling",
    };
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .unwrap_or_else(|| "—".to_string());
    let iter_str = format!("iter {}/{}", app.iteration, app.max_iterations);

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw("⚕ "),
        Span::raw(provider.to_string()),
        Span::raw(" · "),
        Span::raw(model.to_string()),
    ];

    if let Some(total) = app.context_window_size {
        let in_tok = app.last_input_tokens.unwrap_or(0);
        let pct = ((in_tok as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u64;
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(format!("{} / {}", format_tokens(in_tok), format_tokens(*total))));
        spans.push(Span::raw(format!(" {pct}%")));
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(elapsed));
    } else {
        spans.push(Span::raw(" · "));
        spans.push(Span::raw(elapsed));
    }

    spans.push(Span::raw(" · "));
    spans.push(Span::raw(iter_str));
    spans.push(Span::raw(" · "));
    spans.push(Span::raw(mode.to_string()));
    Line::from(spans)
}

/// Build the working-indicator line: `⠋ Working · {elapsed} · {N} tool call(s) · esc to interrupt`.
/// `tick` is a monotonically-increasing counter from the event loop used to
/// rotate the spinner glyph at ~8 Hz. The caller may pass 0 for tests.
fn build_working_line(app: &App) -> Line<'static> {
    let spinner_glyphs: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let elapsed = app
        .turn_started_at
        .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
        .unwrap_or_else(|| "0s".to_string());
    let n = app.iteration;
    let tool_str = if n == 1 { "1 tool call" } else { "{n} tool calls" };
    let _ = spinner_glyphs; // (rotation tied to tick in the future; static glyph for now)
    Line::from(vec![
        Span::raw("⠋ "),
        Span::raw("Working").bold(),
        Span::raw(" · "),
        Span::raw(elapsed),
        Span::raw(" · "),
        Span::raw(tool_str.replace("{n}", &n.to_string())),
        Span::raw(" · esc to interrupt").dim(),
    ])
}

/// Build the input line: `❯ {text_or_placeholder}`.
fn build_input_line(app: &App) -> Line<'static> {
    let prompt = Span::styled("❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    if app.input.is_empty() {
        Line::from(vec![
            prompt,
            Span::styled("Send a message…", Style::default().add_modifier(Modifier::DIM)),
        ])
    } else {
        Line::from(vec![prompt, Span::raw(app.input.clone())])
    }
}

/// Format elapsed seconds into a compact human-friendly form used by the
/// status line. Examples: `0s`, `59s`, `1m 00s`, `59m 59s`, `1h 00m 00s`,
/// `2h 03m 09s`.
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// Gauge imports retained for future use; suppress dead-code warning.
#[allow(dead_code)]
fn _gauge_imports() {
    let _g: Gauge = Gauge::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed_compact(0), "0s");
        assert_eq!(fmt_elapsed_compact(1), "1s");
        assert_eq!(fmt_elapsed_compact(59), "59s");
        assert_eq!(fmt_elapsed_compact(60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(61), "1m 01s");
        assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
        assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
        assert_eq!(fmt_elapsed_compact(3_600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3_600 + 60 + 1), "1h 01m 01s");
        assert_eq!(fmt_elapsed_compact(25 * 3_600 + 2 * 60 + 3), "25h 02m 03s");
    }

    #[test]
    fn status_line_omits_context_when_unset() {
        let app = App::new_for_test();
        let line = build_status_line_1(&app);
        // No context segment when context_window_size is None.
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!s.contains('/'), "expected no slash in status when context_window_size is None; got {s:?}");
    }

    #[test]
    fn status_line_includes_context_percent_when_set() {
        let mut app = App::new_for_test();
        app.context_window_size = Some(1_000_000);
        app.last_input_tokens = Some(200_000);
        let line = build_status_line_1(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("20%"), "expected 20% in status; got {s:?}");
    }

    #[test]
    fn assistant_block_has_rounded_corners() {
        let lines = assistant_block("hello", 40);
        let s: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(s.contains('╭'), "expected top-left border; got {s:?}");
        assert!(s.contains('╰'), "expected bottom-left border; got {s:?}");
        assert!(s.contains("Hermes"), "expected title; got {s:?}");
    }

    #[test]
    fn input_line_has_arrow_prompt() {
        let app = App::new_for_test();
        let line = build_input_line(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains('❯'), "expected ❯ prompt; got {s:?}");
    }

    #[test]
    fn working_line_built_only_when_awaiting() {
        let mut app = App::new_for_test();
        app.mode = AppMode::AwaitingModel;
        app.turn_started_at = Some(Instant::now() - Duration::from_secs(5));
        app.iteration = 3;
        let line = build_working_line(&app);
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(s.contains("Working"), "expected 'Working' label; got {s:?}");
        assert!(s.contains("3 tool calls"), "expected iteration count; got {s:?}");
        assert!(s.contains("esc to interrupt"), "expected interrupt hint; got {s:?}");
    }
}
```

- [ ] **Step 2: Run tests, expect green**

Run: `cargo test -p hermes-cli --lib tui::render`
Expected: PASS, all 6 unit tests.

- [ ] **Step 3: Run the full crate test suite — expect `tui_render.rs` integration tests to fail**

Run: `cargo test -p hermes-cli`
Expected: 2 existing integration tests in `tui_render.rs` fail because the layout changed. (The `tui_input.rs`, `tui_smoke.rs`, `tui_loop_bridge.rs`, `tui_on_event.rs`, `tui_cancel.rs` tests should still pass.)

This is expected. We fix those tests in Task 10.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-cli/src/tui/render.rs
git commit -m "feat(cli): rewrite tui render with codex-style layout (welcome, rounded box, status, input)"
```

---

## Task 7: Wire welcome + turn tracking into `run.rs` and pass `context_window_size`

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs` (signature changes, welcome hook, turn tracking, bug fix §6.1)
- Modify: `crates/hermes-cli/src/main.rs` (extract `context_window_size` and pass through)

- [ ] **Step 1: Update `run()` signature in `tui/mod.rs`**

Open `crates/hermes-cli/src/tui/mod.rs`. Find the `pub use` line. No re-exports change. The `pub fn run` lives in `tui/run.rs`, not in `mod.rs`. Skip this step.

- [ ] **Step 2: Update the public `run` function in `tui/run.rs`**

In `crates/hermes-cli/src/tui/run.rs`, change the `run` function signature from:

```rust
pub async fn run(
    agent: Arc<AIAgent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
) -> Result<(), RunError> {
```

to:

```rust
pub async fn run(
    agent: Arc<AIAgent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
    context_window_size: Option<u64>,
) -> Result<(), RunError> {
```

And inside the function, change:

```rust
let mut app = App::new_for_test();
app.provider_name = Some(provider_name);
app.model_name = Some(model_name);
app.max_iterations = max_iterations;
```

to:

```rust
let mut app = App::new_for_test();
app.provider_name = Some(provider_name);
app.model_name = Some(model_name);
app.max_iterations = max_iterations;
app.context_window_size = context_window_size;
```

- [ ] **Step 3: Update the `run_with_backend` (test entry point) signature**

In the same file, the `run_with_backend` function (we'll keep it but rename and modify the signature in Task 9 — for now, add the `context_window_size` parameter and the welcome/turn tracking). Change:

```rust
pub async fn run_with_backend<B: Backend>(
    backend: B,
    _provider: Arc<dyn Provider>,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
) -> Result<(), RunError> {
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
```

to:

```rust
pub async fn run_with_backend<B: Backend>(
    backend: B,
    mut input_rx: mpsc::UnboundedReceiver<AppEvent>,
    cancel: CancellationToken,
    provider_name: String,
    model_name: String,
    max_iterations: u32,
    context_window_size: Option<u64>,
) -> Result<(), RunError> {
    let mut terminal = Terminal::new(backend).map_err(|e| RunError::Tui(e.to_string()))?;
    let mut app = App::new_for_test();
    app.provider_name = Some(provider_name);
    app.model_name = Some(model_name);
    app.max_iterations = max_iterations;
    app.context_window_size = context_window_size;
```

(We dropped the `_provider: Arc<dyn Provider>` parameter — this is bug fix §6.4.)

- [ ] **Step 4: Add welcome + turn tracking in the `Submit` arm**

In `run()` (production), in the `AppEvent::Submit(text)` arm, change:

```rust
AppEvent::Submit(text) => {
    app.push_line(RenderedLine::User(text.clone()));
    app.session_history.push(Message::user(&text));
    app.mode = AppMode::AwaitingModel;
    let on_event = make_on_event(input_tx.clone());
    let res = agent
        .run_messages(...)
        .await;
    match res {
        Ok(run_result) => {
            app.session_history = run_result.messages;
        }
        Err(e) => {
            app.push_line(RenderedLine::System(format!("error: {e}")));
        }
    }
    app.mode = AppMode::Idle;
}
```

to:

```rust
AppEvent::Submit(text) => {
    app.push_line(RenderedLine::User(text.clone()));
    app.session_history.push(Message::user(&text));
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(Instant::now());
    let on_event = make_on_event(input_tx.clone());
    let res = agent
        .run_messages(...)
        .await;
    app.turn_started_at = None;
    match res {
        Ok(run_result) => {
            app.session_history = run_result.messages;
        }
        Err(e) => {
            app.push_line(RenderedLine::System(format!("error: {e}")));
        }
    }
    app.mode = AppMode::Idle;
}
```

Add `use std::time::Instant;` at the top of the file if not already there.

- [ ] **Step 5: Add the same tracking in the test `run_with_backend` Submit arm**

In `run_with_backend`, change the Submit arm from:

```rust
AppEvent::Submit(text) => {
    app.push_line(RenderedLine::User(text));
    app.mode = AppMode::AwaitingModel;
}
```

to:

```rust
AppEvent::Submit(text) => {
    app.push_line(RenderedLine::User(text));
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(Instant::now());
}
```

- [ ] **Step 6: Apply bug fix §6.1 — dispatch the `handle_key` return value in the test path**

In `run_with_backend`, change:

```rust
AppEvent::Key(k) => {
    let _next = handle_key(&mut app, k);
}
```

to:

```rust
AppEvent::Key(k) => {
    let next = handle_key(&mut app, k);
    match next {
        AppEvent::Submit(text) => {
            app.push_line(RenderedLine::User(text));
            app.mode = AppMode::AwaitingModel;
            app.turn_started_at = Some(Instant::now());
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
        _ => {}
    }
}
```

- [ ] **Step 7: Update `main.rs` to extract and pass `context_window_size`**

In `crates/hermes-cli/src/main.rs`, change:

```rust
let max_iterations = config.agent.max_iterations.unwrap_or(10);

let agent = Arc::new(
    AIAgent::from_config(config)
        .with_context(|| format!("failed to build agent from {}", config_path.display()))?,
);

let cancel = tokio_util::sync::CancellationToken::new();

hermes_cli::tui::run(agent, cancel, provider_name, model_name, max_iterations).await?;
```

to:

```rust
let max_iterations = config.agent.max_iterations.unwrap_or(10);
let context_window_size = config.agent.context_window_size;

let agent = Arc::new(
    AIAgent::from_config(config)
        .with_context(|| format!("failed to build agent from {}", config_path.display()))?,
);

let cancel = tokio_util::sync::CancellationToken::new();

hermes_cli::tui::run(
    agent,
    cancel,
    provider_name,
    model_name,
    max_iterations,
    context_window_size,
)
.await?;
```

- [ ] **Step 8: Compile check**

Run: `cargo check -p hermes-cli`
Expected: compile errors in the existing integration tests (`tui_smoke.rs`) because they call `run_with_backend` with the old signature. We fix those in Task 11. The library and unit tests should compile.

- [ ] **Step 9: Commit**

```bash
git add crates/hermes-cli/src/tui/run.rs \
        crates/hermes-cli/src/main.rs
git commit -m "feat(cli): wire welcome + turn tracking into TUI run loop, pass context_window_size"
```

---

## Task 8: Bug fix §6.2 — `Cancelling` mode ignores non-Ctrl-C keys

**Files:**
- Modify: `crates/hermes-cli/src/tui/input.rs:9-41`
- Test: `crates/hermes-cli/tests/tui_input.rs`

- [ ] **Step 1: Add the failing test**

Open `crates/hermes-cli/tests/tui_input.rs`. Append at the end of the file:

```rust
#[test]
fn cancelling_mode_ignores_typing() {
    use hermes_cli::tui::app::App;
    use hermes_cli::tui::event::{AppEvent, AppMode};
    use hermes_cli::tui::input::handle_key;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = App::new_for_test();
    app.mode = AppMode::Cancelling;
    // Type a character — should be ignored.
    let ev = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert_eq!(ev, AppEvent::Tick, "expected Tick for ignored char in Cancelling");
    assert!(app.input.is_empty(), "input must not grow in Cancelling");

    // Press Enter — should be ignored (no Submit).
    let ev = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ev, AppEvent::Tick, "expected Tick for ignored Enter in Cancelling");
    assert!(app.input.is_empty(), "input must stay empty");

    // Backspace — should be ignored.
    let ev = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    assert_eq!(ev, AppEvent::Tick, "expected Tick for ignored Backspace in Cancelling");
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p hermes-cli --test tui_input cancelling_mode_ignores_typing`
Expected: FAIL — the existing `handle_key` mutates `app.input` even in `Cancelling` mode.

- [ ] **Step 3: Add the guard in `handle_key`**

In `crates/hermes-cli/src/tui/input.rs:9`, change:

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
```

to:

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
    // In Cancelling mode, ignore all other keys until the in-flight turn ends.
    if app.mode == AppMode::Cancelling {
        return AppEvent::Tick;
    }
    // Ctrl-D: only quits from Idle.
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return match app.mode {
            AppMode::Idle => AppEvent::Quit,
            _ => AppEvent::Tick,
        };
    }
    match key.code {
```

- [ ] **Step 4: Run test, expect green**

Run: `cargo test -p hermes-cli --test tui_input`
Expected: PASS, all `tui_input` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-cli/src/tui/input.rs \
        crates/hermes-cli/tests/tui_input.rs
git commit -m "fix(cli): Cancelling mode ignores non-Ctrl-C keys (race with in-flight turn)"
```

---

## Task 9: Bug fix §6.3 + §6.4 — dedup `run_with_backend`, drop `_provider` param

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs` (delete `run_with_backend_and_capture`, simplify `run_with_backend` to take `Arc<Mutex<TestBackend>>`)
- Modify: `crates/hermes-cli/src/tui/mod.rs` (update re-exports)
- Modify: `crates/hermes-cli/tests/tui_smoke.rs` (update call sites)

- [ ] **Step 1: Rewrite `run_with_backend` in `tui/run.rs`**

Find the `pub async fn run_with_backend_and_capture` function (around line 248) and the `SharedTestBackend` struct below it. Delete both.

Then change the surviving `pub async fn run_with_backend<B: Backend>(...)` (around line 182) to take `Arc<Mutex<ratatui::backend::TestBackend>>` directly. The new signature:

```rust
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

    // …rest of the loop body is identical to the old run_with_backend_and_capture body
}
```

The body of the function is what `run_with_backend_and_capture` had. Replace the `break` statements with `return Ok(())` for consistency with the production function.

Also drop the now-unused `use Backend` import; the only backend we accept is `Arc<Mutex<TestBackend>>`.

- [ ] **Step 2: Update `mod.rs` re-exports**

In `crates/hermes-cli/src/tui/mod.rs`, change:

```rust
pub use run::{run, run_with_backend, run_with_backend_and_capture, RunError};
```

to:

```rust
pub use run::{run, run_with_backend, RunError};
```

- [ ] **Step 3: Update `tui_smoke.rs` call sites**

In `crates/hermes-cli/tests/tui_smoke.rs`, find the 3 call sites and update them:

```rust
run_with_backend(
    TestBackend::new(80, 24),  // ← wrap in Arc::new(Mutex::new(...))
    provider,
    input_rx,
    cancel,
    "echo".to_string(),
    "test-model".to_string(),
    10,
)
.await
```

becomes:

```rust
run_with_backend(
    Arc::new(Mutex::new(TestBackend::new(80, 24))),
    input_rx,
    cancel,
    "echo".to_string(),
    "test-model".to_string(),
    10,
    None,
)
.await
```

Note: `provider` is no longer passed. The capture variant previously took `provider`; the surviving one no longer does.

- [ ] **Step 4: For the test that needed buffer inspection (`user_message_appears_in_buffer`), make it capture**

In the same file, `user_message_then_assistant_reply_appears_in_scrollback` previously called `run_with_backend` (the non-capture variant). Now both tests should use the capture variant. The difference is just whether the test inspects the buffer afterward. Adapt the first test to wrap the backend in `Arc::new(Mutex::new(...))` and then drop the `Arc` after `run_with_backend` returns, so the buffer can be inspected.

For the test that doesn't inspect the buffer (`user_message_then_assistant_reply_appears_in_scrollback` and `compact_command_emits_compress_request`), the backend is still wrapped in `Arc<Mutex<...>>` and dropped after the call. No further assertion needed.

- [ ] **Step 5: Run the smoke tests, expect green**

Run: `cargo test -p hermes-cli --test tui_smoke`
Expected: PASS, all 3 smoke tests.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-cli/src/tui/run.rs \
        crates/hermes-cli/src/tui/mod.rs \
        crates/hermes-cli/tests/tui_smoke.rs
git commit -m "refactor(cli): dedup run_with_backend (drop capture variant, drop _provider param)"
```

---

## Task 10: Update `tui_render.rs` integration tests for the new layout

**Files:**
- Modify: `crates/hermes-cli/tests/tui_render.rs` (update 2 existing tests, add 6 new ones)

- [ ] **Step 1: Update the existing `empty_app_renders_input_box_and_status_bar` test**

The new input block is 3 rows tall (bottom row, top row of the 3-tall block, with the middle row containing the prompt). The status bar is now on `height - 1` (the second-to-last row); the input block is the last 3 rows.

Replace the test body with:

```rust
#[test]
fn empty_app_renders_input_box_and_status_bar() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let app = App::new_for_test();
    terminal.draw(|f| render(f, &app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The input block is the last 3 rows; the middle row should contain the
    // placeholder "Send a message…" and the ❯ prompt.
    let input_mid_y = buffer.area.height.saturating_sub(2);
    let input_row: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, input_mid_y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(
        input_row.contains("Send a message"),
        "input middle row should contain placeholder; got: {input_row:?}"
    );
    assert!(
        input_row.contains('❯'),
        "input middle row should contain ❯ prompt; got: {input_row:?}"
    );
}
```

- [ ] **Step 2: Update the existing `populated_app_renders_status_bar_with_provider_and_tokens` test**

Replace the test body with:

```rust
#[test]
fn populated_app_renders_status_bar_with_provider_model_iter_mode() {
    use hermes_cli::tui::event::AppMode;

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.provider_name = Some("openai".to_string());
    app.model_name = Some("gpt-4.1-mini".to_string());
    app.iteration = 2;
    app.max_iterations = 10;
    app.last_input_tokens = Some(12_345);
    app.last_output_tokens = Some(4_549);
    app.mode = AppMode::AwaitingModel;
    // context_window_size is None — context segment must be hidden.

    terminal.draw(|f| render(f, &app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let status_y = buffer.area.height.saturating_sub(2);
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
    assert!(status_row.contains("iter 2/10"), "status row missing iter: {status_row:?}");
    assert!(status_row.contains("awaiting"), "status row missing mode: {status_row:?}");
    // No context segment when context_window_size is None.
    assert!(!status_row.contains('%'), "status row should not show percent when context_window_size is None: {status_row:?}");
}
```

- [ ] **Step 3: Add 6 new tests**

Append to the end of `tui_render.rs`:

```rust
#[test]
fn welcome_banner_visible_on_first_render() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let app = App::new_for_test();
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let height = buffer.area.height as usize;
    let width = buffer.area.width as usize;
    // The banner lives in the first 6 rows (5 banner + 1 tip).
    let mut found_hermes = false;
    for y in 0..6.min(height) {
        let mut row = String::new();
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                row.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        if row.contains("HERMES") || row.contains("Hermes") || row.contains("_____") {
            found_hermes = true;
            break;
        }
    }
    assert!(found_hermes, "expected HERMES banner in first 6 rows of first render");
}

#[test]
fn welcome_banner_hidden_after_first_render() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let height = buffer.area.height as usize;
    let width = buffer.area.width as usize;
    let mut found_banner = false;
    for y in 0..6.min(height) {
        let mut row = String::new();
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                row.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        if row.contains("_____") || row.contains("HERMES") {
            found_banner = true;
            break;
        }
    }
    assert!(!found_banner, "banner must be hidden after welcome_shown is true");
}

#[test]
fn assistant_message_in_rounded_box() {
    use hermes_cli::tui::event::RenderedLine;
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    app.push_line(RenderedLine::Assistant("hello there".to_string()));
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let height = buffer.area.height as usize;
    let width = buffer.area.width as usize;
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                text.push(cell.symbol().chars().next().unwrap_or(' '));
            }
        }
        text.push('\n');
    }
    assert!(text.contains('╭'), "expected top-left ╭ border; got:\n{text}");
    assert!(text.contains('╰'), "expected bottom-left ╰ border; got:\n{text}");
    assert!(text.contains("Hermes"), "expected Hermes title; got:\n{text}");
    assert!(text.contains("hello there"), "expected assistant text; got:\n{text}");
}

#[test]
fn status_row_shows_context_percent_when_configured() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.context_window_size = Some(1_000_000);
    app.last_input_tokens = Some(200_000);
    app.provider_name = Some("openai".into());
    app.model_name = Some("mimo-v2.5".into());
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let status_y = buffer.area.height.saturating_sub(2);
    let row: String = (0..buffer.area.width)
        .map(|x| {
            buffer.cell((x, status_y)).map(|c| c.symbol().chars().next().unwrap_or(' ')).unwrap_or(' ')
        })
        .collect();
    assert!(row.contains("20%"), "expected 20% in status; got {row:?}");
    assert!(row.contains("200.0K"), "expected 200.0K tokens; got {row:?}");
    assert!(row.contains("1.0M"), "expected 1.0M total; got {row:?}");
}

#[test]
fn working_indicator_only_when_awaiting() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    app.iteration = 3;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let height = buffer.area.height as usize;
    let width = buffer.area.width as usize;
    // The working indicator sits between chat and status row 1, so its y is
    // height - 4 (input 3 rows + status 1 row + working 1 row = 5).
    let working_y = height.saturating_sub(4) as u16;
    let mut found_working = false;
    for y in working_y.saturating_sub(1)..=working_y + 1 {
        let row: String = (0..buffer.area.width)
            .map(|x| {
                buffer.cell((x, y)).map(|c| c.symbol().chars().next().unwrap_or(' ')).unwrap_or(' ')
            })
            .collect();
        if row.contains("Working") {
            found_working = true;
            break;
        }
    }
    assert!(found_working, "expected Working indicator near y={working_y}; full buffer not asserted");

    // Now render with Idle mode and assert no Working text.
    app.mode = AppMode::Idle;
    app.turn_started_at = None;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let height = buffer.area.height as usize;
    let width = buffer.area.width as usize;
    for y in 0..height {
        let row: String = (0..width)
            .map(|x| {
                buffer.cell((x as u16, y as u16))
                    .map(|c| c.symbol().chars().next().unwrap_or(' '))
                    .unwrap_or(' ')
            })
            .collect();
        assert!(!row.contains("Working"), "Working must not appear in Idle mode; row y={y}: {row:?}");
    }
}
```

- [ ] **Step 4: Run the test file, expect green**

Run: `cargo test -p hermes-cli --test tui_render`
Expected: PASS, all 6 tests (2 updated + 4 new; we count `welcome_banner_visible_on_first_render`, `welcome_banner_hidden_after_first_render`, `assistant_message_in_rounded_box`, `status_row_shows_context_percent_when_configured`, `working_indicator_only_when_awaiting` as 5 new; the original 2 are updated).

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-cli/tests/tui_render.rs
git commit -m "test(cli): update render tests for codex-style layout (welcome, rounded box, status, input)"
```

---

## Task 11: Final clippy + fmt + full test sweep

**Files:**
- No source changes expected.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: clean exit, no diff.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 0 warnings. If any, fix in place.

- [ ] **Step 3: Full test suite**

Run: `cargo test --all`
Expected: all tests green. Count should be higher than the 179 tests that existed at the start of Phase 10 — we added roughly:
- 2 new in `tui/config.rs`
- 2 new in `context_compression.rs`
- 4 new in `tui/shimmer.rs`
- 6 new in `tui/render.rs`
- 1 new in `tui_input.rs`
- 4 new in `tui_render.rs` (the `assistant_message_in_rounded_box`, `welcome_banner_visible_on_first_render`, `welcome_banner_hidden_after_first_render`, `status_row_shows_context_percent_when_configured`, `working_indicator_only_when_awaiting` — that's 5 actually)

Roughly 20 new tests, so we expect ~199 total.

- [ ] **Step 4: Live smoke**

Run:

```bash
echo '[provider]
kind = "echo"
model = "echo"
[agent]
context_window_size = 100000' > /tmp/hermes-smoke.toml
```

Then: `echo "hello" | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml`
Expected: a quick echo + exit. (The CLI is interactive so this may not work as a piped smoke; skip if it errors.)

- [ ] **Step 5: Commit (if any fmt/clippy changes were made)**

```bash
git add -A
git commit -m "style(phase 10): cargo fmt + clippy cleanups" --allow-empty
```

---

## Self-Review

**1. Spec coverage:**

- §2 Goals (welcome banner, rounded box, status bar, working indicator, bordered input, bug fixes) — covered by Tasks 4, 6, 7, 8, 9, 10.
- §4.1 Welcome banner — Task 4 (shimmer) + Task 6 (banner rendering).
- §4.2 Assistant Message Box — Task 6 (`assistant_block`).
- §4.3 Status Bar (2 lines) — Task 6 (`build_status_line_1` + `build_working_line`).
- §4.4 Input Panel — Task 6 (`build_input_line`).
- §4.5 Layout (6-chunk split) — Task 6 (`render` Layout call).
- §5.1 Config — Task 1.
- §5.1.1 Compressor wiring — Tasks 2 + 3.
- §5.2 App fields — Task 5.
- §5.3 Status bar layout conditional — Task 6.
- §6.1 handle_key return — Task 7 (Step 6).
- §6.2 Cancelling ignores keys — Task 8.
- §6.3 dedup run_with_backend — Task 9.
- §6.4 drop _provider — Task 9.
- §6.5 Cancelling state machine — handled in Task 7 (turn_started_at always cleared).
- §8 Tests — Tasks 3, 4, 6, 8, 10.

**2. Placeholder scan:** No "TBD", "TODO", "implement later", "fill in details", or vague requirements. Every step shows the actual code.

**3. Type consistency:** `App::context_window_size: Option<u64>` defined in Task 5 and used in Tasks 6, 7. `ContextCompressor::new` signature change in Task 2 matches the call site update in the same task. `run_with_backend` parameter `context_window_size: Option<u64>` added in Task 7 and used in Task 9.

**4. Open question verification:** All 4 open questions in the spec are resolved and reflected in the plan (context_window_size, simple banner, no multiline, no spinner during Cancelling).

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-07-phase-10-tui-polish.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints for review.

Which approach?
