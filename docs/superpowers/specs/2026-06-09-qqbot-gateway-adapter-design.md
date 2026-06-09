# QQBot Gateway Adapter Design

## Goal

Add a QQ Bot v2 platform adapter to `perry-hermes-gateway` so users can
interact with the Perry Hermes agent from QQ (C2C private messages and
group @-mentions). The adapter must plug into the existing
`PlatformAdapter` trait alongside Telegram, with a uniform event-conversion
pattern and config flow.

This is a **text-only MVP** — no attachments, no markdown, no inline
keyboards, no media uploads, no multi-account.

## Scope

**In scope:**

- `crates/hermes-gateway/src/qqbot/` module with `QQBotAdapter` (impl
  `PlatformAdapter`).
- `QqBotConfig` (in `src/qqbot/config.rs`) loaded from `GatewayConfig`,
  with configurable env var names.
- Event conversion: `C2cMessage`/`GroupMessage` → `GatewayEvent`.
- Reply routing: `GatewayResponse::Reply(text)` → `Bot::post_*_message`.
- WebSocket transport via `qq-bot-rs` (which already implements
  handshake/heartbeat/Resume/auto-reconnect).
- A small consistency refactor of Telegram: introduce
  `TelegramConfig` so both platforms have a uniform config shape.

**Out of scope (future iterations):**

- Image / voice / file attachments (download + STT)
- Markdown / ark / embed messages
- Inline-keyboard buttons (approval / update-prompt flows)
- Reactions, DMs, share links, message recall
- Webhook transport (we use WebSocket only in v1)
- Multi-account support
- Per-message retry on send failure

## Decisions

| Question | Decision | Rationale |
|---|---|---|
| Transport | **WebSocket** | User-confirmed working as of 2026-06; no public HTTPS required; matches OpenClaw's official QQ plugin and the Python `~/.hermes/hermes-agent` reference. |
| Capability set | **Text in/out only (MVP)** | Matches Telegram adapter's current scope. Defer rich media until core flow is proven. |
| Library | **`qq-bot-rs` as git dependency** | Library is small (4.4k lines), well-tested, has typed events, built-in token cache, reconnect. v0.1.0 is pre-stable but the surface is clean. |
| Workspace version conflicts | **Upgrade workspace to `reqwest 0.13` and `thiserror 2.0`** | Aligned with the lib's own choices; one-time migration cost. |
| Credentials | **`QqBotConfig` struct, env-var backed with configurable names** | User asked for uniform config with telegram — env var names come from `config.toml`. |
| Subscribed intents | **`PUBLIC_MESSAGES`** (C2C + group @) | Covers 90% of usage. |
| Architecture | **Thin wrapper, don't refactor `PlatformAdapter` trait** | The trait is at the right level of abstraction; SDK-specific details (teloxide `Repl` vs `qq-bot-rs::Client::run`) are absorbed inside `PlatformAdapter::run`. |

## Reference Library Profile

`github.com/yenharvey/qq-bot-rs` v0.1.0 — MIT, ~4.4k lines, async/await.

**Public API used by the adapter:**

- `qq_bot_rs::Client` / `ClientBuilder` — entry point
- `qq_bot_rs::Credentials { app_id, app_secret }` — auth
- `qq_bot_rs::Intents::PUBLIC_MESSAGES` — bitflag for our two event types
- `qq_bot_rs::EventHandler` trait — our bridge impl
- `qq_bot_rs::Bot::post_c2c_message` / `post_group_message` — outgoing
- `qq_bot_rs::types::message::{C2cMessage, GroupMessage, OutgoingMessage}` — payload types

**Library already provides** (we don't re-implement):

- WS handshake (Hello → Identify or Resume)
- Heartbeat (op 1) at 80% of server interval
- Token cache + early refresh (singleflight, 9s safety margin)
- Auto-reconnect with backoff on transient close codes
- 4004 invalid-token → token clear + reconnect
- 4006/4007/4009 etc. → session clear + re-identify
- 4914/4915 → fatal, stop retrying
- Strongly-typed events with `Event::Unknown` forward-compat fallback

## File Layout

```
crates/hermes-gateway/
├── Cargo.toml                       (modify: add qq-bot-rs git dep, bump reqwest/thiserror)
├── src/
│   ├── lib.rs                       (modify: pub mod qqbot; re-export)
│   ├── config.rs                    (modify: add QqBotConfig + TelegramConfig to GatewayConfig)
│   ├── adapter.rs                   (unchanged)
│   ├── event.rs                     (unchanged)
│   ├── runner.rs                    (unchanged)
│   ├── telegram/                    (unchanged file layout; add config.rs)
│   │   ├── mod.rs                   (modify: re-export TelegramConfig)
│   │   ├── adapter.rs               (unchanged behavior; use TelegramConfig in ctor)
│   │   └── config.rs                (new: TelegramConfig)
│   └── qqbot/                       (new)
│       ├── mod.rs                   (re-export QQBotAdapter, QqBotConfig)
│       ├── config.rs                (QqBotConfig, QqBotConfigError)
│       ├── adapter.rs               (QQBotAdapter, QqEventBridge)
│       └── events.rs                (c2c_to_event, group_to_event, handle_reply)
└── tests/
    └── (no new integration tests in MVP)
```

## Workspace Cargo Changes

`Cargo.toml` (workspace root):

```toml
[workspace.dependencies]
# upgrades
reqwest  = { version = "0.13", features = ["json", "stream"] }     # was 0.12
thiserror = "2"                                                     # was 1
# additions
qq-bot-rs   = { git = "https://github.com/yenharvey/qq-bot-rs", rev = "<commit-sha>" }
tokio-tungstenite = "0.29"                                          # for lib transitive
bitflags         = { version = "2", features = ["serde"] }         # for lib transitive
```

The `<commit-sha>` placeholder will be resolved at implementation time
by pinning the most recent stable commit of `qq-bot-rs` and recording it
in `Cargo.lock`.

`crates/hermes-gateway/Cargo.toml`:

```toml
[dependencies]
qq-bot-rs.workspace = true
# existing entries unchanged
```

## Component Design

### `QqBotConfig` (`src/qqbot/config.rs`)

```rust
#[derive(Debug, Clone)]
pub struct QqBotConfig {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub app_id_env: String,         // default "QQ_BOT_APP_ID"
    pub app_secret_env: String,     // default "QQ_BOT_APP_SECRET"
    pub sandbox: bool,              // default false
    pub intents: u32,               // default Intents::PUBLIC_MESSAGES bits
}

#[derive(Debug, thiserror::Error)]
pub enum QqBotConfigError {
    #[error("qqbot: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

impl QqBotConfig {
    /// Returns (app_id, app_secret), reading from env if not in config.
    pub fn resolve(&self) -> Result<(String, String), QqBotConfigError>;

    /// Convert raw u32 intents into the lib's typed bitflags.
    pub fn build_intents(&self) -> qq_bot_rs::Intents;
}
```

### `QQBotAdapter` (`src/qqbot/adapter.rs`)

```rust
pub struct QQBotAdapter {
    config: QqBotConfig,
    cancel: CancellationToken,
}

impl QQBotAdapter {
    pub fn new(config: QqBotConfig) -> Self;
}

#[async_trait]
impl PlatformAdapter for QQBotAdapter {
    fn name(&self) -> &str { "qqbot" }

    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()> {
        let (app_id, app_secret) = self.config.resolve()?;
        let intents = self.config.build_intents();

        // Self impls EventHandler — gateway passed in via bridge
        let bridge = QqEventBridge { gateway: Arc::clone(&gateway) };

        let client = qq_bot_rs::Client::builder()
            .credentials(qq_bot_rs::Credentials { app_id, app_secret })
            .intents(intents)
            .handler(bridge)
            .build()?;

        // lib's run() blocks until the WS closes fatally; outer
        // GatewayRunner::run will cancel via disconnect().
        client.run().await
            .map_err(|e| anyhow::anyhow!("qqbot client exited: {e}"))
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.cancel.cancel();
        Ok(())
    }
}
```

### `QqEventBridge` (`src/qqbot/adapter.rs`)

```rust
struct QqEventBridge {
    gateway: Arc<GatewayRunner>,
}

#[async_trait]
impl qq_bot_rs::EventHandler for QqEventBridge {
    async fn on_c2c_message_create(&self, bot: &qq_bot_rs::Bot, msg: C2cMessage) {
        if let Some(ev) = events::c2c_to_event(&msg) {
            events::handle_reply(&self.gateway, &ev, |text| async move {
                let reply = qq_bot_rs::types::OutgoingMessage::text(text);
                bot.post_c2c_message(&msg.author.user_openid, &reply).await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }).await;
        }
    }

    async fn on_group_at_message_create(&self, bot: &qq_bot_rs::Bot, msg: GroupMessage) {
        if let Some(ev) = events::group_to_event(&msg) {
            events::handle_reply(&self.gateway, &ev, |text| async move {
                let reply = qq_bot_rs::types::OutgoingMessage::text(text);
                bot.post_group_message(&msg.group_openid, &reply).await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }).await;
        }
    }
}
```

Other events (`READY`, `RESUMED`, `FRIEND_*`, `C2C_MSG_REJECT`, etc.) use
the lib's default no-op behavior — we do not override them.

### `events` module (`src/qqbot/events.rs`)

```rust
/// Strip "<@!botId> " mention prefix from group content.
fn strip_at_mention(content: &str) -> &str;

pub fn c2c_to_event(msg: &C2cMessage) -> Option<GatewayEvent>;
pub fn group_to_event(msg: &GroupMessage) -> Option<GatewayEvent>;

/// Run a single event through the gateway and ship the reply back via `send`.
pub async fn handle_reply<F, Fut>(gateway: &GatewayRunner, event: &GatewayEvent, send: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
;
```

`c2c_to_event` / `group_to_event` mirror `TelegramAdapter::message_to_event`
in shape: pure function returning `Option<GatewayEvent>`, with the same
field mapping (platform, chat_id, chat_type, user_id, user_name,
message_id, timestamp, text).

### `TelegramConfig` refactor (`src/telegram/config.rs`)

```rust
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub token_env: String,   // default "TELEGRAM_BOT_TOKEN"
}

impl TelegramConfig {
    pub fn resolve(&self) -> Result<String, TelegramConfigError>;
    pub fn build_adapter(&self) -> TelegramAdapter;
}
```

`TelegramAdapter::new` keeps its `(bot_token: &str)` constructor
internally; `TelegramConfig::build_adapter` is the convenience that
existing main() entry points can use.

### `GatewayConfig` (`src/config.rs`)

```rust
pub struct GatewayConfig {
    pub sessions_dir: PathBuf,
    pub working_dir: PathBuf,
    pub allowed_users: HashMap<String, HashSet<String>>,
    pub system_prompt: Option<String>,
    // new:
    pub telegram: Option<TelegramConfig>,
    pub qqbot: Option<QqBotConfig>,
}
```

## Data Flow (per inbound message)

```
[QQ Server]
    │  op 0 Dispatch, t = C2C_MESSAGE_CREATE
    ▼
[qq_bot_rs::Gateway]
    │  stream of DispatchEvent
    ▼
[QqEventBridge::on_c2c_message_create]
    │  C2cMessage
    ▼
[events::c2c_to_event]   ── pure fn ──
    │  GatewayEvent { platform: "qqbot", chat_type: Dm, ... }
    ▼
[GatewayRunner::handle_event]
    │  - authorized check
    │  - command parse (e.g. /reset)
    │  - AIAgent.run_session_turn → streaming text
    │  - collect into response_text
    ▼
[GatewayResponse::Reply(text)]
    │  back in handle_reply
    ▼
[Bot::post_c2c_message] (REST API, async)
    │
    ▼
[QQ Server] → user receives reply
```

Failure modes at each step are logged via `tracing`; the bridge does not
retry (MVP). Token caching, WS reconnect, and Resume are entirely
delegated to the lib.

## Error Handling

| Layer | Error | Handling |
|---|---|---|
| `QqBotConfig::resolve` | missing env / config value | `bail!` from `QQBotAdapter::run` with banner naming the env var the user should set |
| `qq_bot_rs::Client::build` | bad credentials | `bail!` at startup with the lib's error message |
| `client.run` | WS close 4914/4915 (fatal) | `bail!`; `GatewayRunner::run` will log "adapter exited" and stop |
| `client.run` | WS close 4004/4006/4007/4009 | lib auto-recovers, no action needed |
| `client.run` | HTTP 5xx on outgoing send | `tracing::warn!` from `handle_reply`; no retry |
| `gateway.handle_event` | `Unauthorized` | `tracing::warn!`; no reply sent (matches Telegram behavior) |
| `gateway.handle_event` | `AgentRun` | `tracing::warn!`; no reply sent |

## Testing (MVP)

1. **Unit: `events::c2c_to_event` and `events::group_to_event`**
   - Construct `C2cMessage` / `GroupMessage` by hand (lib types derive
     `Deserialize`; we can `serde_json::from_str` from inline fixture
     strings)
   - Assert the produced `GatewayEvent` has expected
     `platform="qqbot"`, `chat_id`, `user_id`, `text`, `message_id`

2. **Unit: `events::strip_at_mention`**
   - `<@!12345> hello world` → `hello world`
   - `no mention here` → `no mention here`
   - Empty string → `""`

3. **Unit: `QqBotConfig::resolve`**
   - `app_id=Some("..."), app_secret=Some("...")` → returns the values
   - Both None, env vars set → returns env values
   - Both None, env vars unset → `Err(MissingCredential { ... })`

4. **Unit: `QqBotConfig::build_intents`**
   - `intents: 0` → defaults to `PUBLIC_MESSAGES`
   - `intents: <custom bits>` → preserves them

5. **No integration test in MVP** — exercised manually with a real
   sandbox bot.

## Documentation

- `README.md` in `crates/hermes-gateway/` — add a "QQBot" subsection to
  the existing usage example showing `QQBotAdapter::new(QqBotConfig::default())`
- Doc comments on each new public type
- Inline comment in `QQBotAdapter::run` explaining the lifecycle
  (lib drives WS, we bridge events to gateway)

## Risks & Mitigations

1. **`qq-bot-rs` API churn** (v0.1.0 is pre-1.0)
   - Pin a specific git rev in `Cargo.toml`
   - Record the rev in code comments and the spec
   - If upstream breaks us, vendor the lib into `crates/qq-bot-rs/`
     (estimated ~2hr migration)

2. **`reqwest 0.13` / `thiserror 2.0` migration touches other crates**
   - Run `cargo check --workspace` after workspace bumps
   - Update derive macros / error types in `hermes-providers`,
     `hermes-agent`, `hermes-core` as needed
   - `reqwest` major bumps usually touch only `.error_for_status()` and
     builder patterns; `thiserror` 1→2 has minor display changes

3. **Long messages**
   - QQ Bot v2 message length cap is ~4000 chars
   - We do **not** chunk in MVP; if a response is truncated, the user
     sees a partial reply
   - Mitigation: a follow-up spec can add chunking (lib has
     `OutgoingMessage::markdown` etc. but plain text is bounded by the
     4000-char protocol limit)

4. **@mention parsing**
   - QQ's at-mention format in group messages is documented as
     `<@!botId> ` — we strip this prefix
   - If the format changes, group messages will surface the raw mention
     to the LLM; not catastrophic but ugly

## Open Questions

None for MVP. All clarified during brainstorming.

## Definition of Done

- [ ] `cargo build --workspace` passes
- [ ] `cargo test -p perry-hermes-gateway` passes (new unit tests green)
- [ ] `QQBotAdapter` implements `PlatformAdapter` with same shape as
      `TelegramAdapter`
- [ ] `QqBotConfig` + `TelegramConfig` parallel each other in
      `GatewayConfig`
- [ ] Manual smoke test against QQ sandbox bot: C2C and group @
      messages round-trip through the gateway
- [ ] `README.md` documents how to enable QQ Bot
- [ ] No new clippy warnings
