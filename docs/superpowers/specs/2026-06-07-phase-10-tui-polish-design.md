# Phase 10 (TUI polish) TUI Polish — Codex-Style Visual Upgrade

**Status:** Draft
**Date:** 2026-06-07
**Branch:** `feature/phase-10-rename-and-tui`
**Depends on:** Phase 10 (commits through `01d78b6`)

## 1. Motivation

The Phase 10 TUI is functional but visually plain. The layout is "chat area + 3-row input + 1-row status bar", every line is a single-line `> foo` / `⚙ bash(...)` / `✓ bash: ...`, and the status bar is one unstyled line of text. The user reported it as "very buggy" and showed a codex-style mockup as the target.

We adopt codex's visual language without copying its architecture. Codex's TUI is 45,326 lines (98 .rs files) — we stay around 1,500 lines and only port the visual primitives that make a difference:

- a shimmer sweep effect on the welcome banner
- rounded-corner message boxes for assistant turns
- a richer two-line status bar (elapsed time, context %, progress bar, spinner)
- a bordered input panel with a `❯` prompt

Concurrently we fix a handful of small bugs surfaced during the Phase 10 final review and a few we'd already noted.

## 2. Goals

1. Welcome banner is a codex-style ASCII art block with a shimmer sweep, rendered exactly once at the start of the TUI.
2. Assistant turns are wrapped in rounded `╭─ Hermes ─...─╮` boxes.
3. Status bar shows provider · model · context (used / total) with a `Gauge` · elapsed · iter X/Y · mode.
4. While `AppMode::AwaitingModel`, a second status line under the bar shows a spinner and the elapsed time formatted as `0s` / `1m 00s` / `1h 00m 00s`.
5. Input is in a bordered `Block` with a `❯ ` prompt prefix; placeholder text reads `Send a message…` and disappears while typing.
6. Bug fixes from §6.

## 3. Non-Goals

- No architectural split into `ChatWidget` / `BottomPane` / `StatusIndicator` (codex's split) — that doubles file count and review surface. All changes stay in `render.rs` + one new `shimmer.rs`.
- No new dependencies. ratatui 0.29 already supports `BorderType::Rounded`, `Gauge`, `Stylize::bold()`/`dim()`, and `Line::from(spans)`.
- No changes to `AgentLoop`, `AIAgent`, `LoopEvent`, `loop_bridge`, `input`, or `event`.
- No changes to slash commands, key handling, or session-history threading.
- **No multiline input.** `Enter` always submits; `Shift+Enter` is a no-op for now (deferred to Phase 12).
- No animation framework — shimmer is one function that recomputes per render based on elapsed-since-start. The existing 60 Hz tick already drives redraws.

## 4. Visual Design

### 4.1 Welcome Banner (one-shot)

Drawn exactly once on the first render of the frame, then suppressed for the rest of the session. The block is centered horizontally within the chat area and surrounded by 1 row of vertical padding.

```
  ▄▄▄▄▄▄▄▄▄▄▄  ▄▄▄▄▄▄▄▄▄▄▄  ▄▄▄▄▄▄▄   ▄▄▄▄▄▄▄▄▄▄▄             ▄▄▄▄▄▄▄
  █░░░░░░░░░▄▄█░░░░░░░░░░░█░░░░░░░░░█░░░░░░░░░█░░░░░░░░░░░░░░░░█░░░░░░░░░█
  █░░▄▀▀▀▀▀░░█░░▄▀▀▀▀▀▀▄▄█░░▄▀▀▀▀▄▀▄░█░░▄▀▀▀▀▀░░█░░▄▀▀▀▀▀▀▄░░░█░░▄▀▀▀▀▀░░░░█
  ...
```

Implementation:

- A multi-line `String` constant `WELCOME_BANNER` in `render.rs` — 5 rows of ASCII art spelling `HERMES`. Reused from codex's `ascii_animation.rs` aesthetic but inlined because we don't ship animation.
- Rendered inside a 6-row-tall `Paragraph` (banner + 1 tip line) at the top of the chat area on the first frame. After drawing, `app.welcome_shown = true`.
- The banner rows have the shimmer sweep applied: a `BOLD` highlight band 5 characters wide sweeping from left to right across the text, 2-second period, synchronized to process start (matches codex's `shimmer_spans`).
- The tip line below the banner is static: `✦ Tip: press / to see available commands.`

### 4.2 Assistant Message Box

Today every assistant line is just raw text. After the change, assistant content is wrapped in:

```
╭─ Hermes ──────────────────────────────────────────╮
│ Hi! I'm Hermes, your coding agent. Ask me         │
│ anything or press / for commands.                 │
╰───────────────────────────────────────────────────╯
```

- `Block::bordered().border_type(BorderType::Rounded).title(" Hermes ")`.
- Title rendered with `Stylize::bold().cyan()` (or default theme accent).
- Text padded by 1 space on the left to clear the `│` border.
- Wrapping handled by `Paragraph::wrap(Word)`.
- Empty assistant lines (delta noise) are folded into the existing in-flight `RenderedLine::Assistant` and re-flowed on the next render — same as today. The box is re-rendered every frame so streaming looks like a smoothly growing rectangle.

### 4.3 Status Bar (always visible, 2 lines)

The bar is now 2 rows. Both rows are rendered with `Style::default().fg(Color::DarkGray)` background to feel "muted".

```
⚕ openai · gpt-4.1-mini · 24.2K / 1M ━━━━━━━░░░░░░░ 2% · 20s · iter 3/10 · awaiting
⠋ Working · 5 tool calls · 1m 12s
```

- Row 1 (always shown): `⚕ {provider} · {model} · {in_tokens} / {ctx_window}  {gauge}  {pct}% · {elapsed} · iter {i}/{max} · {mode}`.
  - `{ctx_window}` default is `1M` (1,000,000). The CLI passes the configured value from `HermesConfig` if present; otherwise 1M.
  - `{gauge}` is a ratatui `Gauge` widget, ratio = `last_input_tokens.unwrap_or(0) / ctx_window`, `0.0..=1.0`. Default style with a `Color::Cyan` filled bar.
  - `{elapsed}` is `fmt_elapsed_compact(turn_started_at.elapsed().as_secs())` — when no turn is in flight, shows `—`.
  - `{mode}` is `idle` / `awaiting` / `cancelling`.
- Row 2 (only when `mode == AwaitingModel`): a single `Line` built from:
  - spinner glyph (`⠋` / `⠙` / `⠹` / `⠸` / `⠼` / `⠴` / `⠦` / `⠧` / `⠇` / `⠏`, rotating at 8 Hz based on tick count)
  - ` ` Working` (with `Stylize::bold()`)
  - ` · {elapsed_compact}` since turn started
  - ` · {N} tool call(s)` (from `app.iteration`)
  - ` · esc to interrupt`

### 4.4 Input Panel

The input is in a 3-row tall bordered block at the bottom of the screen. The cursor sits after a `❯ ` prompt that lives **inside** the block, on the middle row.

```
─────────────────────────────────────────────
❯ what files are in the project?_
─────────────────────────────────────────────
```

- Outer `Block::bordered().border_type(BorderType::Rounded)` with no title.
- Middle row content is a `Line` built as: `Span::styled("❯ ", Style::default().fg(Color::Cyan).bold())` + `Span::raw(input_or_placeholder)`.
- Placeholder text uses `Stylize::dim()`. When input is non-empty, placeholder is hidden and the real text is shown verbatim (no internal styling).
- Multiline input is out of scope — Enter still submits.

### 4.5 Layout

```
┌────────────────── 24 rows available ──────────────────┐
│  (welcome banner — 6 rows, shown once)                │
│  (tip line)                                           │
│                                                        │
│  ╭─ Hermes ─...─╮                                     │
│  │ (assistant)   │                                     │
│  ╰──────────────╯                                     │
│  > user message                                        │
│  ⚙ bash(...)                                          │
│  ✓ bash: ...                                          │
│  ...                                                   │
│                                                        │
│  ⠋ Working · 5 tool calls · 1m 12s                    │  ← row 2 of status
│  ⚕ openai · ... · 2% · 20s · iter 3/10 · awaiting    │  ← row 1 of status
├─ (1 row separator) ──────────────────────────────────┤
│  ╭─────────────────────────────────────────────────╮  │
│  │ ❯ what files are in the project?_                │  │
│  ╰─────────────────────────────────────────────────╯  │
└──────────────────────────────────────────────────────┘
```

Layout constraints (top → bottom):

1. Welcome banner — `Length(6)` when `!app.welcome_shown`, `Length(0)` after.
2. Tip line — `Length(1)` when `!app.welcome_shown`, `Length(0)` after.
3. Chat scrollback — `Min(1)`.
4. Working indicator — `Length(1)` only when `AppMode::AwaitingModel`, else `Length(0)`.
5. Status bar (row 1) — always `Length(1)`.
6. Input block — `Length(3)`.

## 5. App Struct Changes

### 5.1 Config: add `context_window_size` to `AgentConfig`

**File:** `crates/hermes-agent/src/config.rs`

Add one field to `AgentConfig`:

```rust
/// Total context window in tokens for the configured model. Used as the
/// denominator for the percentage-based compression trigger
/// (`context_compression_threshold_percent` × this value = trigger token count),
/// and rendered as the "24.2K / 200K [gauge]" segment in the TUI status bar.
/// When `None`, the compressor falls back to 128_000 (its current default) and
/// the TUI hides the context segment entirely.
#[serde(default)]
pub context_window_size: Option<u64>,
```

Update the `Default` impl likewise. TOML example:

```toml
[agent]
context_window_size = 200_000   # e.g. for a 200K-token model
context_compression_threshold_percent = 0.60
```

### 5.1.1 Wire `context_window_size` through to the compressor

**Why:** `ContextCompressor::new` currently hardcodes `context_length = 128_000` at `crates/hermes-agent/src/context/compressor.rs:299`, so the percentage threshold is always a fraction of 128K regardless of the actual model. We need to thread the configured value through.

**Files:**

- `crates/hermes-agent/src/context/compressor.rs` — change `ContextCompressor::new` to take an `Option<u64>` for the context length:

  ```rust
  pub fn new(config: CompressorConfig, model: String, context_length: Option<u64>) -> Self {
      let context_length = context_length.unwrap_or(128_000);
      let threshold_tok = config.threshold_tokens(context_length);
      // …rest unchanged
  }
  ```

- `crates/hermes-agent/src/runtime_agent.rs` — pass it through:

  ```rust
  ContextCompressor::new(
      compressor_config,
      model_name,
      config.agent.context_window_size,
  )
  ```

- `crates/hermes-agent/tests/context_compression.rs` — update the one call site at `context_compression.rs:105` to pass `None` (preserving prior behavior in that test).

- `crates/hermes-agent/src/context/compressor.rs:537` — update the `update_model_changes_threshold` test similarly.

**Tests for the wiring:**

- A new test in `hermes-agent/tests/context_compression.rs` that constructs an `AIAgent` (or `AgentLoop`) with `context_window_size = Some(200_000)` and `threshold_percent = 0.50`, runs a turn whose `input_tokens` exceeds 100K, and asserts compression triggers. Conversely, with the same 200K window, a turn under 100K does not trigger.
- A regression test asserting that omitting `context_window_size` still triggers at the 128K × threshold (preserves current behavior).

**Behavior when `context_window_size` is `None`:** compressor uses 128_000 (matches today's hardcoded value). TUI status bar hides the context segment. No behavioral surprise.

### 5.2 `App` struct additions

`crates/hermes-cli/src/tui/app.rs` — add three fields:

| Field | Type | Default | Notes |
|---|---|---|---|
| `welcome_shown` | `bool` | `false` | Set to `true` after the first render draws the welcome banner. |
| `turn_started_at` | `Option<Instant>` | `None` | Set to `Some(Instant::now())` when the loop switches to `AwaitingModel`; reset to `None` when it returns to `Idle` or `Cancelling`. |
| `context_window_size` | `Option<u64>` | `None` | Passed from `config.agent.context_window_size`. When `None`, the status bar hides the `{tokens} / {total}  {gauge}  {pct}%` segment entirely. |

No other fields change. The `RenderedLine` enum, `AppMode`, and `AppEvent` are untouched.

`crates/hermes-cli/src/tui/run.rs` — three small edits:

1. In `App::new_for_test()` (already in `app.rs`), nothing to change. In `run()` and `run_with_backend*`, pass `context_window_size` from a new constructor parameter.
2. Set `app.turn_started_at = Some(Instant::now())` when transitioning to `AppMode::AwaitingModel`; clear it when transitioning out.
3. Add `app.context_window_size: Option<u64>` argument to the three `run*` functions and the main `run()` entry point.

`crates/hermes-cli/src/main.rs` — one new line to extract `context_window_size` from `config.agent` and pass it through. The status bar segment is hidden when it's `None`.

### 5.3 Status bar layout, with optional context segment

Row 1 is now conditional:

- **When `context_window_size` is `Some(total)`:**
  ```
  ⚕ openai · gpt-4.1-mini · 24.2K / 200K [gauge 12%] · 20s · iter 3/10 · awaiting
  ```
- **When `context_window_size` is `None`:**
  ```
  ⚕ openai · gpt-4.1-mini · 20s · iter 3/10 · awaiting
  ```

The gauge ratio is `last_input_tokens.unwrap_or(0) / total`, clamped to `0.0..=1.0`. The percentage is rendered as `0%` / `2%` / `100%` (integer, no decimals).

## 6. Bug Fixes (rolled into this phase)

These are bugs the user encountered or that the Phase 10 final review flagged. They're all small; we fix them in the same branch.

### 6.1 `run_with_backend` discards `handle_key` return

**File:** `crates/hermes-cli/src/tui/run.rs:212`

`handle_key` returns an `AppEvent` (e.g. `Quit`, `Submit`, `Compact`). The test path binds it to `_next` and ignores it. The production path correctly pattern-matches the return value. The fix: actually dispatch the returned event like production does.

**Fix:** replace `let _next = handle_key(&mut app, k);` with the same `match next { ... }` block used in production `run()`.

### 6.2 `Cancelling` mode + key submit race

**File:** `crates/hermes-cli/src/tui/input.rs`

While `AppMode::Cancelling`, `Char` and `Enter` keys should not produce `Tick` / `Submit`. Today, the user can keep typing, and once the loop returns from `Cancelling` → `Idle`, a buffered `Submit` could re-trigger. **Fix:** in `handle_key`, when `app.mode == AppMode::Cancelling`, ignore all keys except Ctrl-C (which already maps to `Quit`). Most simply, at the top of `handle_key` for the `Cancelling` arm, return `AppEvent::Tick` for non-Ctrl-C keys.

### 6.3 `run_with_backend` and `run_with_backend_and_capture` are duplicates

**File:** `crates/hermes-cli/src/tui/run.rs:182` and `:248`

Both functions have ~60 lines of identical event-loop logic. The only difference is the `Backend` type. The two-arity version exists so tests can retain a clone of the `Arc<Mutex<TestBackend>>` to inspect the buffer afterward. The simpler version is otherwise dead code — `tui_smoke.rs` uses both, but the only meaningful test (`user_message_appears_in_buffer`) uses the capture variant.

**Fix:** delete `run_with_backend` (the non-capture version). Convert the one `tui_smoke.rs` test that uses it (`user_message_then_assistant_reply_appears_in_scrollback`) to use `run_with_backend_and_capture`. Rename the surviving function back to `run_with_backend`. This removes ~60 lines of duplication.

### 6.4 Dead `_provider` parameter

**File:** `crates/hermes-cli/src/tui/run.rs:184, 250`

`run_with_backend` takes `_provider: Arc<dyn Provider>` that it never uses. The capture variant takes `provider` and ignores it too. Tests still pass `provider` because they always have one handy, but the function never touches it.

**Fix:** drop the `_provider` / `provider` parameter from both function signatures. Update `tui_smoke.rs` and any other call sites to stop passing it.

### 6.5 `mode = Cancelling` is set after `cancel.cancel()` but never cleared to `Idle` on success

**File:** `crates/hermes-cli/src/tui/run.rs:127-129`

When the user presses Ctrl-C during `AwaitingModel`, we set `mode = Cancelling` and call `cancel.cancel()`. If the loop then returns to `AwaitingModel` again (e.g. the cancel was preempted by an in-flight `LoopEvent`), the user is stuck. **Fix:** when the agent returns from a `Submit`, always set `app.mode = AppMode::Idle` unconditionally (which the code already does on the Ok branch at line 123). Audit the `CancelInFlight` arm to clear `app.turn_started_at` and confirm the state machine is correct end-to-end.

## 7. Files Touched

| File | Change |
|---|---|
| `crates/hermes-cli/src/tui/shimmer.rs` | **New.** ~80 lines. Ports codex's `shimmer_spans` (time-sweep, BOLD band) for use on the welcome banner. No new dependencies. |
| `crates/hermes-cli/src/tui/render.rs` | **Rewrite.** ~250 lines. ASCII welcome banner, rounded assistant box, two-line status with `Gauge`, bordered input with `❯`. Adds `fmt_elapsed_compact` helper (15 lines, ported from codex). |
| `crates/hermes-cli/src/tui/app.rs` | **Edit.** Add 3 fields. |
| `crates/hermes-cli/src/tui/run.rs` | **Edit.** Manage `turn_started_at` / `welcome_shown` / fix bugs 6.1, 6.3, 6.4, 6.5. ~40 lines of edits, ~60 lines deleted. |
| `crates/hermes-cli/src/tui/mod.rs` | **Edit.** Add `pub mod shimmer;`. |
| `crates/hermes-cli/src/main.rs` | **Edit.** Extract `context_window_size` from config, pass to `run`. |
| `crates/hermes-cli/tests/tui_render.rs` | **Edit.** Update assertions to the new layout (status row 1 contains `gpt-4.1-mini` and a percentage; bordered input contains `❯`). Add new tests for: welcome banner is drawn exactly once; assistant lines render in a rounded box; status bar shows the working indicator only when `AwaitingModel`; `fmt_elapsed_compact` works. |
| `crates/hermes-cli/tests/tui_smoke.rs` | **Edit.** Convert `user_message_then_assistant_reply_appears_in_scrollback` to use the (renamed) capture variant. Stop passing `provider` to `run_with_backend`. |
| `crates/hermes-cli/tests/tui_input.rs`, `tui_cancel.rs`, `tui_loop_bridge.rs`, `tui_on_event.rs` | **Likely untouched.** The `mode = Cancelling` ignore-keys behavior in §6.2 might add a new assertion to `tui_input.rs`. |
| `docs/superpowers/specs/2026-06-07-phase-10-tui-polish-design.md` | **New.** This file. |
| `docs/superpowers/plans/2026-06-07-phase-10-tui-polish.md` | **New.** Implementation plan. |

No `Cargo.toml` changes. No new dependencies.

## 8. Test Plan

### 8.1 Unit tests in `tui/render.rs`

- `fmt_elapsed_compact(0) == "0s"`, `(59) == "59s"`, `(60) == "1m 00s"`, `(3_600) == "1h 00m 00s"`, `(9_323) == "2h 35m 23s"`. (5 cases.)
- `welcome_lines(app) == [..6..]` when `!app.welcome_shown` and `[]` after.
- `shimmer_spans("Hello")` returns 5 spans; at least one is bold (sweep lands within band on a long enough window).
- `status_row1(app)` contains the formatted string for a populated app.
- `status_row2(app)` is `Some(working_line)` only when `AppMode::AwaitingModel`.

### 8.2 Integration tests in `crates/hermes-cli/tests/tui_render.rs`

- `welcome_banner_visible_on_first_render`: empty app, render to `TestBackend(80, 24)`, assert at least one row contains `HERMES` (case-insensitive substring) or a representative piece of the banner art.
- `welcome_banner_hidden_after_first_render`: same setup, set `app.welcome_shown = true`, assert no banner rows.
- `assistant_message_in_rounded_box`: push a `RenderedLine::Assistant("hello")`, render, assert the row contains the `╭` and `╰` border glyphs and the text `Hermes`.
- `input_block_has_arrow_prompt`: render empty app, assert the input row contains `❯`.
- `status_row_shows_context_percent`: populated app with `last_input_tokens = 200_000`, `context_window_size = 1_000_000`, assert row contains `20%` (or `20.0%`) and a `Gauge` filled portion of the right length.
- `working_indicator_only_when_awaiting`: two apps, one `Idle` and one `AwaitingModel`, both with `turn_started_at` set. Render, assert the `AwaitingModel` buffer has the `Working` text on the row above the bar, and the `Idle` buffer doesn't.
- `cancelling_mode_ignores_typing`: `AppMode::Cancelling`, send `KeyEvent::Char('a')` via `handle_key`, assert the input buffer did **not** grow and the returned event is `Tick`.

### 8.3 Regression

- The full `cargo test --all` and `cargo clippy --all-targets --all-features -- -D warnings` continue to pass.
- `tui_smoke::user_message_appears_in_buffer` still finds `> hi` in the rendered buffer (it should, but if the welcome banner takes vertical space, we may need to scroll the chat area to make room — covered by the layout constraints in §4.5).

## 9. Open Questions (resolved)

1. **Context window.** ~~Default 1M?~~ **Resolved:** read from `[agent].context_window_size` in `hermes.toml`. `None` means hide the segment entirely. (See §5.1.)
2. **Welcome banner art.** ~~Codex-style or simple?~~ **Resolved:** simple 5-row block of `HERMES` letters. (See §4.1.)
3. **Multiline input.** ~~Support `Shift+Enter`?~~ **Resolved:** no. `Enter` always submits. `Shift+Enter` is a no-op (`AppEvent::Tick`) for now. Deferred to Phase 12.
4. **Working indicator during `Cancelling`.** ~~Show spinner?~~ **Resolved:** no spinner. Just the `cancelling` mode word in row 1, no second-line indicator. Keeps the change scope small.

## 10. Out of Scope (Phase 12+)

- ChatWidget / BottomPane / StatusIndicator architectural split (mirroring codex).
- Markdown rendering (codex uses `pulldown-cmark` + `syntect`).
- Slash command autocomplete popup.
- Image paste / multimodal.
- Vim / Emacs keybindings.
- Per-tool result codex-style "exec cell" widgets.
- Theme picker / color schemes.
