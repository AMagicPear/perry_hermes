# QQBot Gateway Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a QQ Bot v2 platform adapter to `perry-hermes-gateway` that ingests text from C2C private messages and group @-mentions, runs them through the Perry Hermes agent, and replies via the QQ REST API. The adapter must plug into the existing `PlatformAdapter` trait alongside Telegram.

**Architecture:** Thin wrapper that bridges `qq_bot_rs` events into `GatewayRunner::handle_event`. WebSocket transport is fully delegated to `qq_bot_rs` (which provides handshake/heartbeat/Resume/auto-reconnect). A small Telegram refactor extracts `TelegramConfig` so both platform configs are symmetric and aggregated under `GatewayConfig`.

**Tech Stack:** Rust 1.95, `tokio 1.x`, `qq-bot-rs` v0.1.0 (git dep), `reqwest 0.13` (upgraded), `thiserror 2.0` (upgraded), `serde`, `tracing`, `chrono`.

**Reference spec:** `docs/superpowers/specs/2026-06-09-qqbot-gateway-adapter-design.md`

---

## File Map (touch list, with responsibilities)

| Path | Action | Responsibility |
|---|---|---|
| `Cargo.toml` | modify | workspace: bump `reqwest` 0.12→0.13, `thiserror` 1→2; add `qq-bot-rs` git dep, `tokio-tungstenite`, `bitflags` |
| `crates/hermes-gateway/Cargo.toml` | modify | add `qq-bot-rs.workspace = true` to dependencies |
| `crates/hermes-gateway/src/lib.rs` | modify | `pub mod qqbot;` and re-exports |
| `crates/hermes-gateway/src/config.rs` | modify | add `TelegramConfig` and `QqBotConfig` fields to `GatewayConfig` |
| `crates/hermes-gateway/src/telegram/mod.rs` | modify | `pub mod config;` re-export `TelegramConfig` |
| `crates/hermes-gateway/src/telegram/config.rs` | create | `TelegramConfig` + `resolve()` + `build_adapter()` |
| `crates/hermes-gateway/src/qqbot/mod.rs` | create | re-export `QQBotAdapter`, `QqBotConfig` |
| `crates/hermes-gateway/src/qqbot/config.rs` | create | `QqBotConfig` + `Default` + `QqBotConfigError` + `resolve()` + `build_intents()` |
| `crates/hermes-gateway/src/qqbot/events.rs` | create | `strip_at_mention()`, `c2c_to_event()`, `group_to_event()`, `handle_reply()` |
| `crates/hermes-gateway/src/qqbot/adapter.rs` | create | `QQBotAdapter`, `QqEventBridge` |
| `crates/hermes-gateway/README.md` | modify | add QQBot usage section |

---

## Task Ordering Rationale

Tasks 1-3 do the workspace dependency bumps first because every later task depends on them. Task 4 extracts `TelegramConfig` to keep the config refactor isolated. Tasks 5-9 build the QQBot adapter bottom-up: `Default`/`resolve()` config first, then pure event-conversion functions, then the `EventHandler` bridge, then the `PlatformAdapter` impl, then `lib.rs` wiring, then README.

---

### Task 1: Bump workspace `reqwest` to 0.13 and `thiserror` to 2.0

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Inspect current workspace `Cargo.toml`**

```bash
grep -n "reqwest\|thiserror" Cargo.toml
```

Expected: `reqwest = { version = "0.12", ... }` and `thiserror = "1"` lines.

- [ ] **Step 2: Edit `Cargo.toml` workspace block**

In the `[workspace.dependencies]` section, change the two lines:

```toml
# before
reqwest = { version = "0.12", features = ["json", "stream"] }
thiserror = "1"
# after
reqwest = { version = "0.13", features = ["json", "stream"] }
thiserror = "2"
```

- [ ] **Step 3: Run workspace check to surface migration issues**

Run: `cargo check --workspace 2>&1 | tail -80`
Expected: One of:
- ✅ Compiles cleanly (unlikely — these are major version bumps)
- ❌ A handful of compile errors in `hermes-providers` / `hermes-agent` / `hermes-core`

Record the exact errors. If any error is **not** auto-resolvable by a simple type change, STOP and ask the user before proceeding.

- [ ] **Step 4: Apply minimal fixes for common migration patterns**

Two patterns cover most of the migration cost. Apply them as they appear:

**`thiserror` 1 → 2**: backward compatible at the derive level. Errors will likely compile unchanged. If `#[error("...")]` format-string changes break, update the format strings.

**`reqwest` 0.12 → 0.13**: check `.error_for_status()` calls (now returns `Err(reqwest::Error)` directly, not `Result<Response, Error>`-after-method). If the codebase uses `.error_for_status_ref()`, no change. Search:

```bash
grep -rn "error_for_status" crates/
```

- [ ] **Step 5: Verify workspace compiles**

Run: `cargo check --workspace 2>&1 | tail -20`
Expected: `Finished` with no errors.

- [ ] **Step 6: Run existing tests to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All pre-existing tests pass.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: bump workspace reqwest 0.13 + thiserror 2.0"
```

---

### Task 2: Add `qq-bot-rs` and transitive deps to workspace

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Look up the latest commit SHA of qq-bot-rs**

Run:
```bash
git ls-remote https://github.com/yenharvey/qq-bot-rs.git HEAD
```

Expected: a 40-character SHA. Copy it.

- [ ] **Step 2: Edit `Cargo.toml` workspace block**

Add three new entries under `[workspace.dependencies]`:

```toml
qq-bot-rs = { git = "https://github.com/yenharvey/qq-bot-rs", rev = "<paste-sha-here>" }
tokio-tungstenite = "0.29"
bitflags = { version = "2", features = ["serde"] }
```

Save the SHA in a code comment in a moment (Task 5 will create the `qqbot` module with a doc comment).

- [ ] **Step 3: Add to `crates/hermes-gateway/Cargo.toml` dependencies**

In `crates/hermes-gateway/Cargo.toml`, in the `[dependencies]` section, add:

```toml
qq-bot-rs.workspace = true
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -30`
Expected: Either ✅ compiles, or a small set of expected errors about `qqbot` module not existing yet (in which case it's fine — Task 5+ will resolve).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hermes-gateway/Cargo.toml
git commit -m "build: add qq-bot-rs git dep + tokio-tungstenite/bitflags transitives"
```

---

### Task 3: Extract `TelegramConfig` for symmetry

**Files:**
- Create: `crates/hermes-gateway/src/telegram/config.rs`
- Modify: `crates/hermes-gateway/src/telegram/mod.rs`
- Modify: `crates/hermes-gateway/src/config.rs`

- [ ] **Step 1: Read current `TelegramAdapter` to know what shape to mirror**

Run: `cat crates/hermes-gateway/src/telegram/adapter.rs | head -20`
Expected: `pub struct TelegramAdapter { bot: Bot }` and `pub fn new(bot_token: &str) -> Self`. No change to the struct or constructor.

- [ ] **Step 2: Create `crates/hermes-gateway/src/telegram/config.rs`**

Write the file:

```rust
//! Telegram platform configuration.

use thiserror::Error;

use super::adapter::TelegramAdapter;

/// Configuration for the Telegram adapter.
///
/// `token` is checked first; if `None`, `token_env` is read from the
/// environment.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: Option<String>,
    pub token_env: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            token: None,
            token_env: "TELEGRAM_BOT_TOKEN".into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TelegramConfigError {
    #[error("telegram: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

impl TelegramConfig {
    /// Returns a valid bot token, reading `token_env` from the env if
    /// `self.token` is `None`.
    pub fn resolve(&self) -> Result<String, TelegramConfigError> {
        if let Some(t) = &self.token {
            return Ok(t.clone());
        }
        match std::env::var(&self.token_env) {
            Ok(v) if !v.is_empty() => Ok(v),
            _ => Err(TelegramConfigError::MissingCredential {
                var: self.token_env.clone(),
            }),
        }
    }

    /// Resolve the token and build a `TelegramAdapter`.
    pub fn build_adapter(&self) -> Result<TelegramAdapter, TelegramConfigError> {
        Ok(TelegramAdapter::new(&self.resolve()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_token() {
        let cfg = TelegramConfig {
            token: Some("explicit".into()),
            token_env: "SHOULD_NOT_BE_READ".into(),
        };
        assert_eq!(cfg.resolve().unwrap(), "explicit");
    }

    #[test]
    fn resolve_falls_back_to_env() {
        // SAFETY: test sets an env var that is not read by any production code
        // path during this test. The name is chosen to be unique.
        unsafe { std::env::set_var("TELEGRAM_TEST_ENV", "from_env") };
        let cfg = TelegramConfig {
            token: None,
            token_env: "TELEGRAM_TEST_ENV".into(),
        };
        assert_eq!(cfg.resolve().unwrap(), "from_env");
        unsafe { std::env::remove_var("TELEGRAM_TEST_ENV") };
    }

    #[test]
    fn resolve_errors_when_neither_set() {
        let cfg = TelegramConfig {
            token: None,
            token_env: "TELEGRAM_NONEXISTENT_VAR_42".into(),
        };
        assert!(matches!(
            cfg.resolve(),
            Err(TelegramConfigError::MissingCredential { .. })
        ));
    }
}
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p perry-hermes-gateway telegram::config 2>&1 | tail -20`
Expected: 3 tests pass.

- [ ] **Step 4: Update `crates/hermes-gateway/src/telegram/mod.rs`**

Replace the file contents with:

```rust
pub mod adapter;
pub mod config;

pub use adapter::TelegramAdapter;
pub use config::{TelegramConfig, TelegramConfigError};
```

- [ ] **Step 5: Add `TelegramConfig` field to `GatewayConfig`**

Read the current `crates/hermes-gateway/src/config.rs` (already known — has `sessions_dir`, `working_dir`, `allowed_users`, `system_prompt`). Edit to add the import and the field:

At the top of the file, after the existing `use` lines, add:

```rust
use crate::telegram::TelegramConfig;
```

In the `GatewayConfig` struct, add the new field (keep existing fields, do not reorder):

```rust
    /// Telegram platform config; `None` disables the adapter.
    pub telegram: Option<TelegramConfig>,
```

In the `Default` impl, add:

```rust
            telegram: None,
```

- [ ] **Step 6: Verify the gateway still compiles**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -20`
Expected: ✅ `Finished`.

- [ ] **Step 7: Run gateway tests**

Run: `cargo test -p perry-hermes-gateway 2>&1 | tail -20`
Expected: All existing tests pass + 3 new TelegramConfig tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/hermes-gateway/src/telegram/config.rs \
        crates/hermes-gateway/src/telegram/mod.rs \
        crates/hermes-gateway/src/config.rs
git commit -m "refactor(gateway): extract TelegramConfig for symmetry with QqBotConfig"
```

---

### Task 4: Create `qqbot::config` with `QqBotConfig` and unit tests

**Files:**
- Create: `crates/hermes-gateway/src/qqbot/config.rs`
- Modify: `crates/hermes-gateway/src/qqbot/mod.rs` (created in Task 5 first; this task is sequential — see step order)

Actually, do this: create the `qqbot/mod.rs` shell first.

- [ ] **Step 1: Create `crates/hermes-gateway/src/qqbot/mod.rs` shell**

```rust
//! QQ Bot platform adapter.

pub mod config;
```

- [ ] **Step 2: Create `crates/hermes-gateway/src/qqbot/config.rs`**

Write the file:

```rust
//! QQ Bot platform configuration.

use thiserror::Error;

/// Configuration for the QQ Bot adapter.
///
/// `app_id` and `app_secret` are checked first; if `None`, the env vars
/// named in `app_id_env` / `app_secret_env` are read.
///
/// `intents` is a raw `u32` bitmask. When 0, [`QqBotConfig::build_intents`]
/// falls back to `Intents::PUBLIC_MESSAGES` (covers C2C + group @).
#[derive(Debug, Clone)]
pub struct QqBotConfig {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub app_id_env: String,
    pub app_secret_env: String,
    pub sandbox: bool,
    pub intents: u32,
}

impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            app_id: None,
            app_secret: None,
            app_id_env: "QQ_BOT_APP_ID".into(),
            app_secret_env: "QQ_BOT_APP_SECRET".into(),
            sandbox: false,
            intents: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum QqBotConfigError {
    #[error("qqbot: {var} env var not set and no value in config")]
    MissingCredential { var: String },
}

impl QqBotConfig {
    /// Returns `(app_id, app_secret)`. Falls back to env vars when
    /// `self.app_id` / `self.app_secret` are `None`.
    pub fn resolve(&self) -> Result<(String, String), QqBotConfigError> {
        let app_id = match &self.app_id {
            Some(v) => v.clone(),
            None => read_env(&self.app_id_env)?,
        };
        let app_secret = match &self.app_secret {
            Some(v) => v.clone(),
            None => read_env(&self.app_secret_env)?,
        };
        Ok((app_id, app_secret))
    }

    /// Convert `self.intents` into the lib's typed bitflags.
    /// `intents == 0` falls back to `Intents::PUBLIC_MESSAGES`.
    pub fn build_intents(&self) -> qq_bot_rs::Intents {
        if self.intents == 0 {
            qq_bot_rs::Intents::PUBLIC_MESSAGES
        } else {
            // Safe: `intents` is a u32, same as Intents::bits.
            qq_bot_rs::Intents::from_bits_truncate(self.intents)
        }
    }
}

fn read_env(var: &str) -> Result<String, QqBotConfigError> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(QqBotConfigError::MissingCredential { var: var.into() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_values() {
        let cfg = QqBotConfig {
            app_id: Some("id123".into()),
            app_secret: Some("secret456".into()),
            ..QqBotConfig::default()
        };
        assert_eq!(cfg.resolve().unwrap(), ("id123".into(), "secret456".into()));
    }

    #[test]
    fn resolve_falls_back_to_env() {
        unsafe { std::env::set_var("QQ_BOT_TEST_ID", "id_from_env") };
        unsafe { std::env::set_var("QQ_BOT_TEST_SECRET", "secret_from_env") };
        let cfg = QqBotConfig {
            app_id: None,
            app_secret: None,
            app_id_env: "QQ_BOT_TEST_ID".into(),
            app_secret_env: "QQ_BOT_TEST_SECRET".into(),
            ..QqBotConfig::default()
        };
        let (id, secret) = cfg.resolve().unwrap();
        assert_eq!(id, "id_from_env");
        assert_eq!(secret, "secret_from_env");
        unsafe { std::env::remove_var("QQ_BOT_TEST_ID") };
        unsafe { std::env::remove_var("QQ_BOT_TEST_SECRET") };
    }

    #[test]
    fn resolve_errors_when_missing() {
        let cfg = QqBotConfig {
            app_id_env: "QQ_BOT_NONEXISTENT_VAR_42".into(),
            app_secret_env: "QQ_BOT_NONEXISTENT_VAR_42".into(),
            ..QqBotConfig::default()
        };
        assert!(matches!(
            cfg.resolve(),
            Err(QqBotConfigError::MissingCredential { .. })
        ));
    }

    #[test]
    fn build_intents_defaults_to_public_messages() {
        let cfg = QqBotConfig::default();
        let intents = cfg.build_intents();
        assert!(intents.contains(qq_bot_rs::Intents::PUBLIC_MESSAGES));
    }

    #[test]
    fn build_intents_preserves_custom_bits() {
        let cfg = QqBotConfig {
            intents: 0b1010,
            ..QqBotConfig::default()
        };
        let intents = cfg.build_intents();
        assert_eq!(intents.bits(), 0b1010);
    }
}
```

- [ ] **Step 3: Add `qq-bot-rs` to hermes-gateway Cargo.toml if not already there**

The previous task added it, but verify:

Run: `grep "qq-bot-rs" crates/hermes-gateway/Cargo.toml`
Expected: a `qq-bot-rs.workspace = true` line. If missing, add it.

- [ ] **Step 4: Run the new config tests**

Run: `cargo test -p perry-hermes-gateway qqbot::config 2>&1 | tail -30`
Expected: 5 tests pass.

If `Intents::from_bits_truncate` or `Intents::PUBLIC_MESSAGES` is not the actual API in the version pinned:

- Look at the lib's `Intents` type: `cargo doc -p qq-bot-rs --no-deps 2>&1 | tail -5` then open the rendered docs, or
- Read `src/intents.rs` from the clone at `/tmp/qq-bot-rs-investigate/src/intents.rs`
- The actual API is `bitflags`-style with `from_bits_truncate(u32) -> Self` and a `bits() -> u32` accessor, and a constant `Intents::PUBLIC_MESSAGES` (verified during spec investigation).

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-gateway/src/qqbot/
git commit -m "feat(gateway): add QqBotConfig with env-var fallback and intents helper"
```

---

### Task 5: Add `QqBotConfig` to `GatewayConfig`

**Files:**
- Modify: `crates/hermes-gateway/src/config.rs`

- [ ] **Step 1: Edit `config.rs` to import and add the field**

At the top of `crates/hermes-gateway/src/config.rs`, add an import below the existing `use` lines:

```rust
use crate::qqbot::QqBotConfig;
```

In the `GatewayConfig` struct, add the field (alongside the `telegram` field from Task 3):

```rust
    /// QQ Bot platform config; `None` disables the adapter.
    pub qqbot: Option<QqBotConfig>,
```

In the `Default` impl, add:

```rust
            qqbot: None,
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -10`
Expected: ✅ Finished.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-gateway/src/config.rs
git commit -m "feat(gateway): add qqbot field to GatewayConfig"
```

---

### Task 6: Implement `events::strip_at_mention` and unit test

**Files:**
- Modify: `crates/hermes-gateway/src/qqbot/mod.rs`
- Create: `crates/hermes-gateway/src/qqbot/events.rs`

- [ ] **Step 1: Write the failing test first**

Create `crates/hermes-gateway/src/qqbot/events.rs`:

```rust
//! Conversion from `qq_bot_rs` events to the gateway's `GatewayEvent`.

use qq_bot_rs::types::message::{C2cMessage, GroupMessage};

use crate::event::{ChatType, GatewayEvent};
use crate::runner::GatewayRunner;

/// Strip `<@!botId> ` mention prefix from group message content.
///
/// QQ's protocol embeds the bot mention at the start of group @ messages.
/// We strip the prefix so the LLM sees clean text.
fn strip_at_mention(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("<@!") {
        if let Some(space_idx) = rest.find(' ') {
            return &rest[space_idx + 1..];
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_at_mention_with_mention() {
        assert_eq!(strip_at_mention("<@!12345> hello world"), "hello world");
    }

    #[test]
    fn strip_at_mention_without_mention() {
        assert_eq!(strip_at_mention("no mention here"), "no mention here");
    }

    #[test]
    fn strip_at_mention_empty() {
        assert_eq!(strip_at_mention(""), "");
    }

    #[test]
    fn strip_at_mention_partial_prefix_only() {
        // No space after the bot id — leave unchanged.
        assert_eq!(strip_at_mention("<@!12345>"), "<@!12345>");
    }
}
```

- [ ] **Step 2: Update `qqbot/mod.rs` to declare `events`**

Edit `crates/hermes-gateway/src/qqbot/mod.rs` to:

```rust
//! QQ Bot platform adapter.

pub mod config;
pub mod events;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p perry-hermes-gateway qqbot::events 2>&1 | tail -20`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-gateway/src/qqbot/
git commit -m "feat(gateway): qqbot strip_at_mention helper"
```

---

### Task 7: Implement `c2c_to_event` and `group_to_event` with tests

**Files:**
- Modify: `crates/hermes-gateway/src/qqbot/events.rs`

- [ ] **Step 1: Add the two conversion functions and their tests**

Append the following to `crates/hermes-gateway/src/qqbot/events.rs` (just before the closing of the `tests` module's outer scope — i.e., add the new functions and tests; do not remove the existing `strip_at_mention` test module):

```rust
/// Parse QQ's ISO 8601 timestamp string into `chrono::DateTime<Utc>`.
///
/// Returns `Utc::now()` on parse failure (the message is still routed; the
/// timestamp is metadata only).
fn parse_qq_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

/// Convert a `C2cMessage` to a `GatewayEvent`.
///
/// Returns `None` if the message has no text (e.g. attachment-only).
pub fn c2c_to_event(msg: &C2cMessage) -> Option<GatewayEvent> {
    let text = msg.content.trim();
    if text.is_empty() {
        return None;
    }
    Some(GatewayEvent {
        platform: "qqbot".into(),
        chat_id: msg.author.user_openid.clone(),
        chat_type: ChatType::Dm,
        user_id: msg.author.user_openid.clone(),
        user_name: None,
        thread_id: None,
        text: text.to_string(),
        message_id: Some(msg.id.clone()),
        timestamp: parse_qq_timestamp(&msg.timestamp),
    })
}

/// Convert a `GroupMessage` to a `GatewayEvent`.
///
/// Strips the leading `<@!botId> ` mention and returns `None` if the
/// remaining text is empty.
pub fn group_to_event(msg: &GroupMessage) -> Option<GatewayEvent> {
    let text = strip_at_mention(&msg.content).trim();
    if text.is_empty() {
        return None;
    }
    Some(GatewayEvent {
        platform: "qqbot".into(),
        chat_id: msg.group_openid.clone(),
        chat_type: ChatType::Group,
        user_id: msg.author.member_openid.clone(),
        user_name: None,
        thread_id: None,
        text: text.to_string(),
        message_id: Some(msg.id.clone()),
        timestamp: parse_qq_timestamp(&msg.timestamp),
    })
}
```

Add tests inside the existing `tests` module. The lib's `C2cMessage` / `GroupMessage` types `derive(Deserialize, PartialEq)`, so we can construct them via `serde_json`:

```rust
    use qq_bot_rs::types::message::{C2cMessageAuthor, GroupMessageAuthor};
    use serde_json::json;

    #[test]
    fn c2c_to_event_maps_fields() {
        let msg: C2cMessage = serde_json::from_value(json!({
            "id": "MSG_ID_1",
            "author": { "user_openid": "U_OPENID_1" },
            "content": "hello there",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        let ev = c2c_to_event(&msg).expect("event");
        assert_eq!(ev.platform, "qqbot");
        assert_eq!(ev.chat_id, "U_OPENID_1");
        assert_eq!(ev.user_id, "U_OPENID_1");
        assert!(matches!(ev.chat_type, ChatType::Dm));
        assert_eq!(ev.text, "hello there");
        assert_eq!(ev.message_id.as_deref(), Some("MSG_ID_1"));
    }

    #[test]
    fn c2c_to_event_returns_none_for_empty_text() {
        let msg: C2cMessage = serde_json::from_value(json!({
            "id": "X",
            "author": { "user_openid": "U" },
            "content": "   ",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        assert!(c2c_to_event(&msg).is_none());
    }

    #[test]
    fn group_to_event_maps_fields_and_strips_mention() {
        let msg: GroupMessage = serde_json::from_value(json!({
            "id": "G_MSG_1",
            "group_openid": "G_OPENID_1",
            "author": { "member_openid": "M_OPENID_1" },
            "content": "<@!BOTID> ping the bot",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        let ev = group_to_event(&msg).expect("event");
        assert_eq!(ev.chat_id, "G_OPENID_1");
        assert_eq!(ev.user_id, "M_OPENID_1");
        assert!(matches!(ev.chat_type, ChatType::Group));
        assert_eq!(ev.text, "ping the bot");
        assert_eq!(ev.message_id.as_deref(), Some("G_MSG_1"));
    }

    #[test]
    fn group_to_event_returns_none_when_only_mention() {
        let msg: GroupMessage = serde_json::from_value(json!({
            "id": "G_MSG_2",
            "group_openid": "G",
            "author": { "member_openid": "M" },
            "content": "<@!BOTID> ",
            "attachments": [],
            "timestamp": "2026-06-09T08:00:00+00:00"
        }))
        .unwrap();
        assert!(group_to_event(&msg).is_none());
    }
```

Note: the `C2cMessageAuthor` and `GroupMessageAuthor` imports are only needed inside `tests` — Rust will accept the `use` inside the `mod tests { ... }` block. Make sure these are inside that block, not at the top of the file.

- [ ] **Step 2: Run the tests**

Run: `cargo test -p perry-hermes-gateway qqbot::events 2>&1 | tail -30`
Expected: 8 tests pass total (4 from Task 6 + 4 new).

If a test fails with "missing field `timestamp`" or similar: the lib's `C2cMessage` / `GroupMessage` struct requires `timestamp` (per `inbound.rs`, `timestamp: String` is non-optional). The test fixtures already include it. If another field is required, add it to the `json!` macros.

- [ ] **Step 3: Commit**

```bash
git add crates/hermes-gateway/src/qqbot/events.rs
git commit -m "feat(gateway): qqbot event conversion (C2C + group)"
```

---

### Task 8: Implement `handle_reply` helper

**Files:**
- Modify: `crates/hermes-gateway/src/qqbot/events.rs`

- [ ] **Step 1: Add `handle_reply` and a test**

Append to the end of `events.rs`, outside the `tests` module:

```rust
/// Run a single `GatewayEvent` through the gateway and ship the reply
/// back via the provided async `send` closure.
///
/// Failures are logged via `tracing`; the bridge does not retry.
pub async fn handle_reply<F, Fut>(gateway: &GatewayRunner, event: &GatewayEvent, send: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    match gateway.handle_event(event.clone()).await {
        Ok(crate::runner::GatewayResponse::Reply(text)) => {
            if let Err(e) = send(text).await {
                tracing::warn!(error = %e, "qqbot: send reply failed");
            }
        }
        Ok(crate::runner::GatewayResponse::Ignored) => {}
        Err(e) => {
            tracing::warn!(error = %e, "qqbot: gateway error");
        }
    }
}
```

Add a test inside the `tests` module. We need a way to drive `handle_reply` without a real gateway. The simplest approach: build a minimal mock that implements nothing — but `handle_reply` needs a real `&GatewayRunner`.

Alternative: split this task. Write a `handle_reply`-friendly helper that accepts a closure for "process the event", and unit-test that closure path. The actual `handle_reply` is then a one-liner that wires it up.

Simpler approach: skip the unit test for `handle_reply` (the integration is thin — it just calls `gateway.handle_event` and `send`). The behavior is exercised by the manual smoke test in the Definition of Done. Trust the code; the test for it isn't pulling its weight.

So: do **not** add a test for `handle_reply`. Just write the function and verify it compiles.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -10`
Expected: ✅ Finished.

- [ ] **Step 3: Run all gateway tests**

Run: `cargo test -p perry-hermes-gateway 2>&1 | tail -20`
Expected: All existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-gateway/src/qqbot/events.rs
git commit -m "feat(gateway): qqbot handle_reply helper"
```

---

### Task 9: Implement `QQBotAdapter` and `QqEventBridge`

**Files:**
- Create: `crates/hermes-gateway/src/qqbot/adapter.rs`
- Modify: `crates/hermes-gateway/src/qqbot/mod.rs`

- [ ] **Step 1: Create `crates/hermes-gateway/src/qqbot/adapter.rs`**

```rust
//! QQ Bot v2 platform adapter.
//!
//! Uses [`qq_bot_rs`] for the WebSocket transport (handshake, heartbeat,
//! resume, auto-reconnect) and bridges the typed events into the gateway's
//! [`GatewayRunner::handle_event`].

use std::sync::Arc;

use async_trait::async_trait;
use qq_bot_rs::types::message::{C2cMessage, GroupMessage, OutgoingMessage};

use crate::adapter::PlatformAdapter;
use crate::qqbot::config::QqBotConfig;
use crate::runner::GatewayRunner;

/// Adapter that runs a QQ Bot v2 client and dispatches inbound events.
pub struct QQBotAdapter {
    config: QqBotConfig,
}

impl QQBotAdapter {
    pub fn new(config: QqBotConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl PlatformAdapter for QQBotAdapter {
    fn name(&self) -> &str {
        "qqbot"
    }

    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()> {
        let (app_id, app_secret) = self.config.resolve()?;
        let intents = self.config.build_intents();

        let bridge = QqEventBridge {
            gateway: Arc::clone(&gateway),
        };

        // Sandbox mode routes REST + WS to the QQ sandbox environment.
        // The lib's BotBuilder distinguishes via .sandbox(true).
        let bot = qq_bot_rs::Bot::builder()
            .sandbox(self.config.sandbox)
            .build(qq_bot_rs::Credentials::new(app_id, app_secret))?;

        let client = qq_bot_rs::Client::builder()
            .bot(bot)
            .intents(intents)
            .handler(bridge)
            .build()?;

        // lib's run() blocks until the WS closes (transient close codes
        // are auto-recovered internally; fatal codes 4914/4915 exit).
        // For MVP we don't try to cancel mid-run; lib's reconnect handles
        // transient drops. disconnect() is a no-op.
        client.run().await.map_err(|e| anyhow::anyhow!("qqbot client exited: {e}"))
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        // No-op for MVP. lib's run() will exit on the next fatal WS close.
        // A future iteration can add a oneshot::Sender<()> shutdown channel
        // plumbed into QqEventBridge if mid-run cancel is needed.
        Ok(())
    }
}

/// Bridges `qq_bot_rs::EventHandler` callbacks into the gateway.
struct QqEventBridge {
    gateway: Arc<GatewayRunner>,
}

#[async_trait]
impl qq_bot_rs::EventHandler for QqEventBridge {
    async fn on_c2c_message_create(&self, bot: &qq_bot_rs::Bot, msg: C2cMessage) {
        let Some(ev) = super::events::c2c_to_event(&msg) else {
            return;
        };
        let user_openid = msg.author.user_openid.clone();
        super::events::handle_reply(&self.gateway, &ev, move |text| async move {
            let reply = OutgoingMessage::text(text);
            bot.post_c2c_message(&user_openid, &reply)
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .await;
    }

    async fn on_group_at_message_create(&self, bot: &qq_bot_rs::Bot, msg: GroupMessage) {
        let Some(ev) = super::events::group_to_event(&msg) else {
            return;
        };
        let group_openid = msg.group_openid.clone();
        super::events::handle_reply(&self.gateway, &ev, move |text| async move {
            let reply = OutgoingMessage::text(text);
            bot.post_group_message(&group_openid, &reply)
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .await;
    }
}
```

- [ ] **Step 2: Update `qqbot/mod.rs`**

```rust
//! QQ Bot platform adapter.

pub mod adapter;
pub mod config;
pub mod events;

pub use adapter::QQBotAdapter;
pub use config::{QqBotConfig, QqBotConfigError};
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -30`
Expected: Either ✅ Finished, or compile errors. The most likely candidates:

- `qq_bot_rs::Credentials::new(app_id, app_secret)` — verify this is the constructor. If the lib uses a struct literal instead, change to `Credentials { app_id, app_secret }`. Confirm by reading `/tmp/qq-bot-rs-investigate/src/auth.rs`.
- `bot.post_c2c_message(...)` / `post_group_message(...)` — return `Result<SentMessage, HttpError>`. The `map_err(|e| anyhow::anyhow!("{e}"))` should work since `HttpError: Display`.
- `OutgoingMessage::text(text)` — confirm constructor name. Check `/tmp/qq-bot-rs-investigate/src/types/message/outgoing.rs` for the actual method name (likely `text`, possibly `new`).
- `qq_bot_rs::Intents::PUBLIC_MESSAGES` — already verified.

For any of these that don't match, fix the code, re-run `cargo check`.

- [ ] **Step 4: Run all gateway tests**

Run: `cargo test -p perry-hermes-gateway 2>&1 | tail -20`
Expected: All existing tests pass (no new tests in this task — `QQBotAdapter` is exercised via the manual smoke test in the Definition of Done).

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-gateway/src/qqbot/
git commit -m "feat(gateway): QQBotAdapter with C2C + group @ message routing"
```

---

### Task 10: Re-export from `lib.rs`

**Files:**
- Modify: `crates/hermes-gateway/src/lib.rs`

- [ ] **Step 1: Read the current `lib.rs`**

The current `lib.rs` (verified during brainstorming) declares `pub mod telegram;` and re-exports `TelegramAdapter`. Mirror that pattern for `qqbot`.

- [ ] **Step 2: Add `qqbot` module declaration and re-exports**

In the `pub mod` section (after `pub mod telegram;`), add:

```rust
pub mod qqbot;
```

In the `pub use` section (after the Telegram re-exports), add:

```rust
pub use qqbot::{QQBotAdapter, QqBotConfig, QqBotConfigError};
```

Update the doc comment at the top of the file to mention QQ Bot in the same breath as Telegram:

Replace the existing crate-level doc (the first paragraph) with:

```rust
//! Platform gateway for Perry Hermes.
//!
//! This crate provides the gateway layer that bridges messaging platforms
//! (Telegram, QQ Bot, Discord, etc.) with the Perry Hermes agent runtime. It
//! centralizes session management, message routing, and platform adapter
//! dispatch.
```

In the `# Usage` example, add a QQ Bot line after the Telegram one (keep both):

```rust
//! # fn example(agent: Arc<perry_hermes_agent::AIAgent>) {
//! let config = GatewayConfig::default();
//! let runner = GatewayRunner::new(agent, config);
//! let telegram = Arc::new(TelegramAdapter::new("BOT_TOKEN"));
//! let qqbot = Arc::new(QQBotAdapter::new(QqBotConfig::default()));
//! // runner.run(vec![telegram, qqbot]).await;
//! # }
```

- [ ] **Step 3: Verify compile and tests**

Run: `cargo check -p perry-hermes-gateway 2>&1 | tail -10`
Expected: ✅ Finished.

Run: `cargo test -p perry-hermes-gateway 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-gateway/src/lib.rs
git commit -m "feat(gateway): re-export QQBotAdapter + QqBotConfig from lib root"
```

---

### Task 11: Update README with QQ Bot usage

**Files:**
- Modify: `crates/hermes-gateway/README.md` (if present; create if absent)

- [ ] **Step 1: Locate the README**

Run: `ls crates/hermes-gateway/README* 2>&1`
Expected: Either an existing `README.md` or no match.

- [ ] **Step 2: If `README.md` exists, add a "QQBot" subsection after the existing "Telegram" usage**

Append (or insert) a section:

```markdown
### QQBot

```rust,no_run
use std::sync::Arc;
use perry_hermes_gateway::{GatewayConfig, GatewayRunner, QQBotAdapter, QqBotConfig};

# async fn example(agent: Arc<perry_hermes_agent::AIAgent>) -> anyhow::Result<()> {
let config = GatewayConfig::default();
let runner = GatewayRunner::new(agent, config);
let qqbot = Arc::new(QQBotAdapter::new(QqBotConfig::default()));
// runner.run(vec![qqbot]).await?;
# Ok(())
# }
```

Set the `QQ_BOT_APP_ID` and `QQ_BOT_APP_SECRET` environment variables
before running. To use the QQ sandbox environment instead, set
`QqBotConfig { sandbox: true, ..QqBotConfig::default() }`. The adapter
uses the `qq-bot-rs` WebSocket transport; no public HTTPS endpoint is
required.
```

- [ ] **Step 3: If no `README.md` exists, create one with the existing usage example and the new QQBot subsection**

Skip if the file already exists and has a Telegram section — just add QQBot to it.

- [ ] **Step 4: Verify the doc tests still build (if doctests exist)**

Run: `cargo test -p perry-hermes-gateway --doc 2>&1 | tail -20`
Expected: Either ✅ (no doctests) or pass.

- [ ] **Step 5: Commit**

```bashgit add crates/hermes-gateway/README.md
git commit -m "docs(gateway): add QQBot usage section to README"
```

---

### Task 12: Final verification

- [ ] **Step 1: Workspace compile**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: ✅ `Finished`.

- [ ] **Step 2: All tests pass**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All tests pass, no failures.

- [ ] **Step 3: Clippy clean**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30`
Expected: No new warnings. If there are pre-existing warnings, verify they are not from new code.

- [ ] **Step 4: Confirm no stray TODOs or FIXMEs in the new code**

Run: `grep -rn "TODO\|FIXME\|XXX" crates/hermes-gateway/src/qqbot/ crates/hermes-gateway/src/telegram/config.rs`
Expected: No matches.

- [ ] **Step 5: Manual smoke test plan (executed by human, not by this agent)**

Document this in the PR description; not run as part of this plan.

Prerequisites for smoke test:
- A QQ Open Platform sandbox bot (AppID + AppSecret)
- `QQ_BOT_APP_ID` and `QQ_BOT_APP_SECRET` env vars set
- `QQ_BOT_SANDBOX=true` if using sandbox

Procedure:
1. Build: `cargo build --release -p perry-hermes-gateway`
2. Run a small example that wires `QQBotAdapter` into a `GatewayRunner`
3. Send a C2C message from QQ → confirm it echoes / responds
4. Send a group @-mention → confirm it responds in the group
5. Verify `/reset` and `/status` commands work (already handled by the gateway)

- [ ] **Step 6: Commit any final fixes (likely none)**

```bash
git status
# If clean, no commit needed. Otherwise:
# git add <files>
# git commit -m "chore: final cleanup"
```

---

## Definition of Done (cross-check with spec)

- [x] `cargo build --workspace` passes — Task 12 step 1
- [x] `cargo test -p perry-hermes-gateway` passes — Task 12 step 2
- [x] `QQBotAdapter` implements `PlatformAdapter` with same shape as `TelegramAdapter` — Tasks 9 + 10
- [x] `QqBotConfig` + `TelegramConfig` parallel each other in `GatewayConfig` — Tasks 3 + 5
- [x] Manual smoke test against QQ sandbox bot — Task 12 step 5 (human-executed)
- [x] `README.md` documents how to enable QQ Bot — Task 11
- [x] No new clippy warnings — Task 12 step 3

## Common Pitfalls

1. **`reqwest::Error` change in 0.13**: `.error_for_status()` no longer returns a `Result<Response, Error>`-shaped thing — but if you only call `.error_for_status()` for its side effect (early return on non-2xx), the migration is automatic.

2. **`Intents` is `bitflags!`-derived**: `Intents::PUBLIC_MESSAGES` is a `const`, not a method. The `from_bits_truncate` constructor takes a `u32`; passing `0` is fine but loses all bits — for that reason Task 4 routes `0` through a fallback to the default.

3. **The lib's `C2cMessage` / `GroupMessage` use ISO 8601 string timestamps**, not `DateTime<Utc>`. The `parse_qq_timestamp` helper in `events.rs` does the conversion. A parse failure falls back to `Utc::now()` — the timestamp is metadata, not message content.

4. **The `qq_bot_rs::Bot::builder().build(creds)` API**: `build()` takes the credentials as a final argument, not chained. This differs from `Client::builder().credentials(...)` which is chained. Don't mix them up.

5. **`unsafe { std::env::set_var(...) }` in tests**: `std::env::set_var` is marked `unsafe` since Rust 1.84 or so (effects on other threads). Wrap in `unsafe { ... }` blocks. The tests in this plan already do this.

6. **The `lib.rs` doctest**: The example in the crate-level doc uses `TelegramAdapter::new("BOT_TOKEN")` which still works since the constructor is unchanged. The new line adds `QQBotAdapter::new(QqBotConfig::default())` for parity.

## Out-of-scope Reminders (do not implement)

- Image / voice / file attachments
- Markdown / ark / embed messages
- Inline-keyboard buttons (`INTERACTION_CREATE` — event is logged at debug by the lib)
- Reactions, DMs, share links, message recall
- Webhook transport
- Multi-account support
- Per-message retry on send failure
- Mid-run cancellation of the QQ Bot client
