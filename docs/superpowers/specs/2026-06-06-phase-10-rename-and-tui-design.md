# Phase 10: Rename `hermes-skills` → `hermes-skill-loader` + Replace `hermes-cli` REPL with `ratatui` TUI

**Date:** 2026-06-06
**Status:** Proposed
**Scope:** Two related cleanups that together close the existing Phase 10 roadmap item and prepare the codebase for Phase 11 (gateway adapters).

## 1. Goal

1. Rename the leaf data crate `hermes-skills` → `hermes-skill-loader` so the crate name reflects its actual responsibility (loading + validating SKILL.md files and rendering the prompt-injection block). Nothing else moves; content, public API, and downstream callers stay functionally identical.
2. Replace the `hermes-cli` REPL with a `ratatui`-based TUI. The legacy REPL code is **discarded** — there is no `--tui` toggle and no fallback. The new `hermes-cli` binary drives a full-screen event loop that consumes `LoopEvent`s from `AIAgent` and renders them as scrollable chat history with a status bar.
3. Make the "gateway abstraction" story explicit: `AIAgent` is the shared composition point that both the local TUI and the future `hermes-gateway` adapters (Phase 11) consume.

## 2. Non-Goals

- A separate `hermes-tui` crate. The TUI lives **inside** `hermes-cli`; introducing a new top-level crate would add a workspace member with no new responsibility.
- Mouse support, multi-pane layouts, plugin renderers. Phase 10 is a single-pane chat view with a status bar.
- Persistent session storage (save/load chat history across runs). The REPL did not have it; the TUI does not add it.
- Mouse-driven resizing, copy/paste from the OS clipboard, and other terminal conveniences that depend on the host terminal. Phase 10 supports standard `ratatui` keyboard input only.
- Adding `--tui`/`--repl` flags or a config toggle to switch between modes. The REPL is gone.
- Changing the public API of `AIAgent` or `AgentLoop`. The TUI is a consumer of the existing `run_messages(...)` interface.
- The `hermes-gateway` crate itself. This spec only renames the workspace member slot comment in `Cargo.toml` to drop the `hermes-tui` placeholder and keep the `hermes-gateway` placeholder for Phase 11.

## 3. Background

### 3.1 Why the rename

`hermes-skills` is a leaf crate (~900 LOC) that handles only data: SKILL.md frontmatter parsing, directory layout discovery, validation, and the system-prompt metadata block renderer. The runtime **tools** that operate on skills (`SkillListTool`, `SkillViewTool`, `SkillLinkedFilesTool`) live in `hermes-agent/src/tools/skills/` because they implement the `Tool` trait and depend on `hermes-core`.

The current name `hermes-skills` overlaps with the LLM-callable tools in `tools/skills/`, which is confusing — a casual reader could think the two are the same thing. Renaming the data crate to `hermes-skill-loader` makes the data-vs-tool split self-documenting:

- `hermes-skill-loader` — loads and validates SKILL.md, produces the prompt block
- `hermes-agent::tools::skills::*` — runtime tools the LLM can call to explore loaded skills

This is **content-preserving**; no logic moves. The crate directory is renamed, the package name in `Cargo.toml` is changed, and every `use hermes_skills::...` is updated to `use hermes_skill_loader::...`.

### 3.2 Why the REPL → TUI rewrite (and not a separate `hermes-tui` crate)

The current `hermes-cli` REPL is a simple line-based loop that streams tokens, prints tool events, and dispatches `/quit`/`/exit`/`/compact`. It works, but it is showing its age:

- Token streaming appears as a single growing line; tool events appear inline and scroll past without trace.
- The user has no way to scroll back through earlier messages without re-running the session.
- Status information (provider name, model, current iteration, token usage) is invisible unless the agent decides to print it.

A `ratatui`-based TUI gives us a chat-style scrollback, a status bar, and a clear input area, all driven by the same `LoopEvent` stream the REPL already consumes. The TUI is a **denser renderer** of the same events, not a new abstraction over the agent.

Adding a new `hermes-tui` crate would split the product shell across two crates with no clean boundary between them. The TUI and the CLI arg/config-resolution code share state, share `clap` parsing, share config resolution, and share the same startup. Keeping them together in `hermes-cli` is the smaller, more cohesive change.

### 3.3 Why we discard the REPL outright (no `--tui` flag)

We considered keeping the REPL behind a `--tui` flag for users on terminals that cannot run `ratatui` (some embedded terminals, very old `xterm`s). The reasons we discard it instead:

- `ratatui` already has a robust `TestBackend` and a small set of fallback behaviors for legacy terminals. In practice it runs everywhere a modern interactive shell does.
- A dual-mode binary doubles the rendering and event-handling code paths, which is exactly the kind of "fragmentation inside one crate" the Phase 9 architecture-cohesion refactor pushed back on.
- The REPL is not a stable, user-visible contract that we need to preserve. The TUI is a strict superset of features (it can render the same events the REPL could).

## 4. Target Architecture

### 4.1 Crate layout

After this phase:

```text
crates/
├── hermes-core
├── hermes-providers
├── hermes-skill-loader      # ← renamed from hermes-skills
├── hermes-agent
├── hermes-cli               # TUI inside; no hermes-tui crate
└── hermes-gateway           # Phase 11, slot reserved
```

`Cargo.toml` updates:

- Remove `"crates/hermes-skills"` from `members`, add `"crates/hermes-skill-loader"`.
- Remove the `hermes-tui` placeholder comment; keep the `hermes-gateway` placeholder for Phase 11.

### 4.2 `hermes-skill-loader` (renamed, no content change)

Public API stays the same. The crate's `lib.rs` doc comment and the doc-comment that currently references the future `SkillActivationTool` get one cosmetic update to reflect the new name. Specifically:

- `crates/hermes-skill-loader/src/lib.rs` header: drop the path to the old design doc (which never existed under that exact filename); keep the module-level docstring but rephrase the "Phase 12 deliverable" sentence to use the new crate name.
- All `use hermes_skills::...` call sites in `hermes-agent`, `hermes-cli`, and tests become `use hermes_skill_loader::...`.
- The `prompting.rs` system-prompt composer inside `hermes-agent` (which already imports from `hermes_skills`) is updated mechanically.

### 4.3 `hermes-cli` TUI

The new `hermes-cli` keeps the same `clap` argument surface (`--config`, `--cwd`, `--quiet`, etc.) and the same config-resolution logic. The body of the program, however, becomes a `ratatui` event loop.

#### 4.3.1 Module layout

```text
crates/hermes-cli/src/
├── main.rs              # clap parse, config resolve, handoff to tui::run()
├── config_path.rs       # unchanged
├── ctrl_c.rs            # unchanged (still used to wire SIGINT into a CancellationToken)
├── cli_render.rs        # REMOVED (replaced by tui module)
├── repl.rs              # REMOVED
├── tui/
│   ├── mod.rs           # pub fn run(agent, session, cancel) -> Result<()>
│   ├── app.rs           # App state machine: input buffer, scrollback, status
│   ├── input.rs         # KeyEvent → AppEvent mapping (slash commands, Ctrl-C, Ctrl-D)
│   ├── render.rs        # Frame painter: chat area, status bar, input box
│   └── event.rs         # AppEvent enum, internal messages
└── tests/
    └── tui_render.rs    # uses ratatui::backend::TestBackend
```

The legacy `cli_render.rs` and `repl.rs` files are deleted. Their content is fully superseded by the TUI modules.

#### 4.3.2 TUI app state

```rust
pub struct App {
    pub scrollback: Vec<RenderedLine>,   // chat history
    pub input: InputBuffer,              // current user input
    pub status: StatusBar,               // provider, model, tokens, iteration
    pub mode: AppMode,                   // Idle | AwaitingModel | Cancelling
    pub session_history: Vec<Message>,  // messages handed to/from AIAgent
    pub session: SessionContext,
    pub cancel: CancellationToken,
}

pub enum AppMode {
    Idle,                // user is typing
    AwaitingModel,       // agent is streaming; input is disabled
    Cancelling,          // first Ctrl-C received; second exits
}

pub enum RenderedLine {
    User(String),
    Assistant(String),         // grows as tokens stream in
    Reasoning(String),         // optional, dimmed
    ToolCall { name: String, args_preview: String },
    ToolResult { name: String, output: String, ok: bool },
    System(String),            // compression events, errors, slash-command feedback
}
```

#### 4.3.3 Event loop

`App::run` is a `tokio::select!` over three sources:

1. **Crossterm terminal events** (key presses) → `AppEvent::Key(KeyEvent)`.
2. **The `on_event` callback** passed to `AIAgent::run_messages` → `AppEvent::Loop(LoopEvent)`. The TUI owns the callback and translates `LoopEvent`s into `RenderedLine`s or status updates before storing them in `App`.
3. **A `tokio::time::interval` tick** at 60 Hz → `AppEvent::Tick` to drive redraws while the agent is streaming.

When the user presses Enter in `Idle` mode, the TUI:

1. parses the input as a slash command or a regular user message;
2. switches to `AwaitingModel` and disables the input box;
3. appends `RenderedLine::User(text)` to the scrollback;
4. calls `agent.run_messages(updated_history, &session, cancel.clone(), on_event).await`;
5. on completion, replaces `session_history` with `run_result.messages` and switches back to `Idle`.

#### 4.3.4 Slash commands

Slash commands stay first-class and are handled inside the TUI's input layer:

- `/quit` and `/exit` — set a "shutdown" flag the loop observes on the next tick; the loop drops out and `main` exits cleanly.
- `/compact` and `/compact <focus>` — translate to `AIAgent::run_compact(focus)` (or its Phase 7 equivalent) and render the resulting `LoopEvent::CompressionCompleted` / `CompressionSkipped` in the status bar.
- `/clear` — clears the on-screen scrollback and the session history. (New in Phase 10; useful for the TUI's chat-style view.)

Unknown slash commands render a `RenderedLine::System` line and do not consume a turn.

#### 4.3.5 Cancellation

- First `Ctrl-C` while `AwaitingModel` cancels the in-flight `run_messages` call via the existing `CancellationToken`. The TUI re-enables input and renders a `RenderedLine::System("⚠ cancelled")` line.
- Second `Ctrl-C` in any mode exits the TUI.
- `Ctrl-D` exits the TUI from `Idle` mode only.
- The legacy `repl.rs` Ctrl-C handling is gone; `ctrl_c.rs` is kept because it owns the `tokio::signal::ctrl_c` → `CancellationToken` plumbing the TUI still needs.

#### 4.3.6 Status bar

The status bar at the bottom of the screen shows, left-to-right:

- Provider kind and model name (from `HermesConfig` and the provider name stashed on `AIAgent`).
- Current iteration count and `max_iterations` (updated on each `LoopEvent::LoopIteration`).
- Token usage: `in: 12.3K  out: 4.5K  ctx: 38%` (read from the most recent `LoopEvent::LoopIteration` carrying usage; the TUI does not re-estimate tokens itself).
- Current `AppMode` as a colored chip.

When a `CompressionCompleted` event arrives, the status bar flashes a one-line hint (`🗜️ Compressed: 142K → 38K tokens in 1.2s`) for 2 seconds, matching the REPL's old behavior.

### 4.4 Dependency direction (unchanged)

```text
hermes-cli (TUI)
  └─ hermes-agent
       ├─ hermes-core
       ├─ hermes-providers
       └─ hermes-skill-loader
```

The TUI is a consumer; it adds no new top-level edges. It does add `ratatui` + `crossterm` to `hermes-cli/Cargo.toml`.

## 5. Migration Plan

The plan is intentionally sequenced so each step is independently mergeable and testable.

1. **Rename `hermes-skills` → `hermes-skill-loader`.** Mechanical: `git mv crates/hermes-skills crates/hermes-skill-loader`, edit package name, update all `use` statements and `Cargo.toml` members, run `cargo build` and `cargo test`. No public-API change.
2. **Add `ratatui` + `crossterm` to `hermes-cli/Cargo.toml`.** No code yet; just a manifest change so we can see compile time in isolation.
3. **Build the TUI scaffolding.** Create `crates/hermes-cli/src/tui/{mod,app,input,render,event}.rs` with empty `App` and a `run()` that draws an empty `Frame`. Wire it from `main.rs` instead of the existing REPL entry point. Discard `repl.rs` and `cli_render.rs` in the same commit.
4. **Wire `on_event` to the TUI.** Pass a closure into `AIAgent::run_messages` that converts `LoopEvent`s into `AppEvent::Loop` and sends them through a `tokio::sync::mpsc` channel. The TUI's event loop receives them on the same `select!` as key events.
5. **Implement input handling** for plain typing, Enter to send, slash commands, and `Ctrl-C`/`Ctrl-D` semantics.
6. **Implement rendering** of the scrollback and status bar.
7. **Test with the `echo` provider end-to-end.** A scripted integration test (`crates/hermes-cli/tests/tui_smoke.rs`) drives the TUI through a `ratatui::backend::TestBackend`, sends a turn, and asserts the rendered `Buffer` contains the expected lines.
8. **Update `Cargo.toml` placeholder comments** to drop the `hermes-tui` line.

## 6. Testing

### 6.1 Unit

- `tui::input` — slash-command parsing, key bindings.
- `tui::render` — pure function from `App` snapshot to `Buffer`; assertions on cell content for a few representative states (empty, mid-stream, error, compacted).
- `tui::app` — state transitions: `Idle → AwaitingModel → Cancelling → Idle`.

### 6.2 Integration (TestBackend)

- `tui_smoke.rs` — start the TUI with a `ScriptedProvider`, send a turn, assert that:
  - the input line clears after Enter,
  - the user message appears in the scrollback,
  - the streaming assistant text appears as it arrives,
  - the final state shows the `EchoProvider` response in the status bar's model field.
- `tui_cancel.rs` — drive an in-flight turn and trigger `CancellationToken::cancel()`; assert the TUI re-enables input and renders the cancellation system line.
- `tui_compact.rs` — trigger `/compact` mid-session; assert the status bar shows the compression hint within 2 seconds.

### 6.3 Manual smoke

- `cargo run -p hermes-cli` against the `echo` provider: type, see the response, scroll, `/quit`.
- `cargo run -p hermes-cli` against a real provider: streaming tokens appear without flicker, status bar updates as usage arrives, `Ctrl-C` cancels a long-running tool call.

## 7. Open Questions / Risks

- **Backwards-incompatible CLI change.** Anyone scripting against the current REPL (e.g., piping a single message via stdin) will need to switch to the TUI's pipe mode (Phase 10 ships no pipe mode; the `printf 'hello' | hermes-cli` smoke test from `CLAUDE.md` no longer applies). We accept this in exchange for a single product shell.
- **`ratatui` on Windows.** The TUI uses `crossterm`, which works on Windows but requires the terminal to be in the right mode. This is a `crossterm` concern, not a hermes concern; documented in the README.
- **Terminal resize handling.** The TUI should redraw on `SIGWINCH`. `crossterm` exposes this as a `KeyEvent::Resize(w, h)` variant; we handle it in the input layer.
- **Long output truncation.** A tool result longer than the chat area's height is currently rendered as a single line and clipped. We will add a `j`/`k` or PgUp/PgDn shortcut to scroll individual long tool outputs. This is a polish item; it ships with Phase 10 if simple, otherwise as a follow-up.
- **No `--tui` fallback.** If a user's terminal genuinely cannot run `ratatui`, the TUI will fail at startup with a clear error. We will not maintain a REPL escape hatch.

## 8. LOC Budget

| Component | LOC | Notes |
|---|---:|---|
| Rename `hermes-skills` → `hermes-skill-loader` | ~50 (mechanical) | `git mv`, Cargo.toml, use statements |
| `tui::event` + `tui::input` | ~250 | key binding table, slash-command parser |
| `tui::app` | ~400 | state machine, history merge logic |
| `tui::render` | ~350 | scrollback, status bar, input box |
| `tui::mod` (entry point + select!) | ~150 | event sources, lifecycle |
| Delete `repl.rs` + `cli_render.rs` | −400 | net negative |
| TUI tests | ~600 | TestBackend scenarios across the three integration files |
| **Net production code** | **~+750** | |
| **Net tests** | **~+600** | |

The TUI is a net addition over the REPL, but the additional code is largely the rendering layer (which the REPL did not have) and the structured state machine. There is no new agent-side complexity.

## 9. What This Spec Drops From the Roadmap

- The `hermes-tui` crate placeholder in `Cargo.toml` is removed. We considered a separate crate; this spec argues against it and consolidates into `hermes-cli`.
- The `--tui` flag is removed. There is one mode.
- A potential `--repl` flag is removed. There is no fallback.
