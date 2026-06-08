# Gateway + Telegram Adapter — Implementation Plan

## Overview

Create `perry-hermes-gateway` as a **library crate** that centralizes platform
management and session handling for all non-CLI platforms. The first platform
adapter is Telegram. The main CLI binary (`perry-hermes-cli`) gains a `gateway`
subcommand to launch it.

## Architecture (aligned with Python hermes-agent)

```text
perry-hermes-cli (binary: perry-hermes)
  ├── (default)       → interactive TUI (existing)
  └── gateway         → start all configured platform adapters
                        └── uses perry-hermes-gateway library
                            ├── GatewayRunner (orchestrator)
                            ├── SessionRegistry (session lifecycle)
                            ├── PlatformAdapter trait
                            └── telegram::TelegramAdapter
```

## Message Flow (mirrors Python GatewayRunner)

```
Platform API → Adapter normalizes to GatewayEvent
  → GatewayRunner.handle_event()
    → build_session_key(platform, chat_id, user_id)
    → SessionRegistry.get_or_create(session_key)
    → Acquire per-session turn_lock (serialize concurrent messages)
    → agent.run_session_turn(text, &session, cancel, on_event)
    → Collect LoopEvent::ContentDelta → final response
    → adapter.send_message(chat_id, response)
    → SessionRegistry persist
```

## Crate Structure

```text
crates/perry-hermes-gateway/
  Cargo.toml
  src/
    lib.rs                     — public API re-exports
    config.rs                  — GatewayConfig, PlatformConfig, SessionResetPolicy
    event.rs                   — GatewayEvent (normalized incoming message)
    session_registry.rs        — SessionRegistry: DashMap + per-session Mutex
    adapter.rs                 — PlatformAdapter trait
    runner.rs                  — GatewayRunner: orchestrator (like Python's GatewayRunner)
    telegram/
      mod.rs
      adapter.rs               — TelegramAdapter: teloxide long-poll
      commands.rs              — /reset, /compact, /status
```

## Key Types

### `PlatformAdapter` trait (like Python BasePlatformAdapter)

```rust
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Platform identifier ("telegram", "discord", etc.)
    fn name(&self) -> &str;

    /// Start receiving messages. For each incoming message, call
    /// `gateway.handle_event(event)`. Blocks until shutdown.
    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()>;

    /// Send a text response back to a chat.
    async fn send_message(&self, chat_id: &str, text: &str) -> anyhow::Result<()>;

    /// Show typing indicator (best-effort, non-blocking).
    async fn send_typing(&self, chat_id: &str) -> anyhow::Result<()>;

    /// Gracefully disconnect.
    async fn disconnect(&self) -> anyhow::Result<()>;
}
```

### `GatewayEvent` (like Python MessageEvent)

```rust
pub struct GatewayEvent {
    pub platform: String,         // "telegram"
    pub chat_id: String,          // platform-specific chat identifier
    pub chat_type: ChatType,      // Dm | Group | Channel | Thread
    pub user_id: String,          // platform-specific user identifier
    pub user_name: Option<String>,
    pub thread_id: Option<String>, // forum topic / thread
    pub text: String,
    pub message_id: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

### `SessionKey` (like Python build_session_key)

Format: `{platform}:{chat_type}:{chat_id}[:{thread_id}][:user:{user_id}]`

- DMs: `telegram:dm:123456`
- Groups (shared): `telegram:group:-100123456`
- Groups (per-user): `telegram:group:-100123456:user:789`
- Threads: `telegram:group:-100123456:thread:42`

Built by `SessionRegistry::build_key(event)`.

### `SessionEntry`

```rust
pub struct SessionEntry {
    pub session: AgentSession,
    pub turn_lock: Mutex<()>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_active: chrono::DateTime<chrono::Utc>,
}
```

### `SessionRegistry`

```rust
pub struct SessionRegistry {
    sessions: DashMap<String, SessionEntry>,
    sessions_dir: PathBuf,
    agent: Arc<AIAgent>,
}

impl SessionRegistry {
    pub async fn get_or_create(&self, key: &str) -> SessionEntry;
    pub async fn reset(&self, key: &str);
    pub async fn save_all(&self);
}
```

Session creation: `AgentSession::new(key, working_dir, system_message).with_json_file_store(sessions_dir.join("{key}.json"))`.

### `GatewayRunner` (like Python GatewayRunner)

```rust
pub struct GatewayRunner {
    agent: Arc<AIAgent>,
    sessions: SessionRegistry,
    config: GatewayConfig,
    adapters: Vec<Arc<dyn PlatformAdapter>>,
}

impl GatewayRunner {
    pub async fn handle_event(&self, event: GatewayEvent) -> Result<String, GatewayError>;
    pub async fn run(&self) -> anyhow::Result<()>;
    pub async fn shutdown(&self);
}
```

`handle_event()`:
1. Authorization check against `config.allowed_users`
2. Build session key from event
3. Get or create session from registry
4. Acquire per-session turn_lock
5. Call `agent.run_session_turn(text, &session, cancel, on_event)`
6. Collect response from LoopEvent stream
7. Return response text

`run()`:
1. Call `adapter.run(gateway)` on all adapters concurrently via `tokio::select!`
2. On shutdown signal, call `adapter.disconnect()` on all adapters

### `GatewayConfig`

```rust
pub struct GatewayConfig {
    pub sessions_dir: PathBuf,
    pub working_dir: PathBuf,
    /// Key: platform name, Value: allowed user IDs. Empty = allow all.
    pub allowed_users: HashMap<String, HashSet<String>>,
    pub reset_triggers: Vec<String>,  // ["/new", "/reset"]
}
```

## Telegram Adapter

Uses `teloxide` for Telegram Bot API long-polling.

**TelegramAdapter** implements `PlatformAdapter`:
- `run()`: starts teloxide update stream, for each message:
  - Build `GatewayEvent` from `teloxide::types::Message`
  - Check commands (/reset, /compact, /status)
  - Call `gateway.handle_event(event)`
  - Send response via `bot.send_message()`
- `send_message()`: teloxide `bot.send_message(chat_id, text).await`
- `send_typing()`: teloxide `bot.send_chat_action(chat_id, ChatAction::Typing).await`
- `disconnect()`: signal shutdown

**Session key for Telegram:**
- DM: `telegram:dm:{chat_id}`
- Group: `telegram:group:{chat_id}`
- Forum topic: `telegram:group:{chat_id}:thread:{thread_id}`

**Commands** (in-process, no separate module needed initially):
- `/reset` → `sessions.reset(key)` → reply "Session reset"
- `/compact` → `agent.compact_session(&session, None)` → reply result
- `/status` → session info (message count, token usage)

## Workspace Changes

### Root `Cargo.toml`
- Uncomment `"crates/perry-hermes-gateway"` in members
- Add to `[workspace.dependencies]`:
  ```toml
  teloxide = { version = "0.13", features = ["macros"] }
  dashmap = "6"
  ```

### `crates/hermes-cli/Cargo.toml`
- Add `perry-hermes-gateway` dependency

### `crates/hermes-cli/src/main.rs`
- Add `Gateway` subcommand:
  ```rust
  #[derive(Subcommand)]
  enum Command {
      /// Interactive TUI (default)
      Tui,
      /// Start platform gateway
      Gateway,
  }
  ```
- `perry-hermes gateway` → load config → build AIAgent → build GatewayRunner → run

## Files to Create

| # | File | Content |
|---|------|---------|
| 1 | `crates/perry-hermes-gateway/Cargo.toml` | Crate manifest |
| 2 | `crates/perry-hermes-gateway/src/lib.rs` | Re-exports |
| 3 | `crates/perry-hermes-gateway/src/config.rs` | GatewayConfig, PlatformConfig |
| 4 | `crates/perry-hermes-gateway/src/event.rs` | GatewayEvent, ChatType |
| 5 | `crates/perry-hermes-gateway/src/session_registry.rs` | SessionRegistry, SessionEntry, build_key |
| 6 | `crates/perry-hermes-gateway/src/adapter.rs` | PlatformAdapter trait |
| 7 | `crates/perry-hermes-gateway/src/runner.rs` | GatewayRunner |
| 8 | `crates/perry-hermes-gateway/src/telegram/mod.rs` | Module re-exports |
| 9 | `crates/perry-hermes-gateway/src/telegram/adapter.rs` | TelegramAdapter |
| 10 | `crates/perry-hermes-gateway/src/telegram/commands.rs` | Command handling |

## Files to Modify

| # | File | Change |
|---|------|--------|
| 1 | Root `Cargo.toml` | Uncomment gateway, add deps |
| 2 | `crates/hermes-cli/Cargo.toml` | Add gateway dep |
| 3 | `crates/hermes-cli/src/main.rs` | Add `gateway` subcommand |

## Testing

- **SessionRegistry**: get-or-create, concurrent access, key generation, reset
- **GatewayRunner::handle_event**: EchoProvider, verify full message flow
- **Commands**: /reset, /compact, /status with mock agent
- **TelegramAdapter**: command parsing, event building; no real API calls
