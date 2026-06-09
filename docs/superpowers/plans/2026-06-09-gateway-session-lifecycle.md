# Gateway Session Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire session persistence into `SessionRegistry::get_or_create` so a gateway restart preserves the conversation history of a Telegram or QQ chat, and add an archive-on-reset / archive-on-CLI-exit lifecycle that supports a future `/resume` command and forward-compatible sub-agent contexts.

**Architecture:** Two new fields on `AgentSession` (`parent_session_id`, `role`) make the schema forward-compatible without a migration. A new `archive_to` method on `AgentSession` plus a new `archive_active` method on `SessionRegistry` move the active JSON file to `sessions/.archive/<key>/<utc_ts>.json` and clear the in-memory history. `get_or_create` learns to load from the existing JSON file (or recover from a corrupt one) instead of always starting empty. The gateway runner and CLI shutdown call `archive_active` at the right points.

**Tech Stack:** Rust 1.95, tokio, serde_json, chrono, dashmap (existing).

---

## File Structure

Files modified in this plan, with their one-line responsibility:

| File | Responsibility |
|---|---|
| `crates/hermes-agent/src/session.rs` | `AgentSession` schema + `archive_to` method. |
| `crates/hermes-agent/src/session_registry.rs` | Load-on-`get_or_create`, corrupt recovery, `archive_active`, `create_sub_session`, archive-then-reset. |
| `crates/hermes-gateway/src/runner.rs` | `/reset` archives; `/status` reports archive count. |
| `crates/hermes-cli/src/tui/run.rs` | Archive on clean shutdown. |
| `crates/hermes-gateway/tests/session_lifecycle.rs` (new) | Integration test for restart continuity. |

No new public crates, no new dependencies.

---

## Task 1: Add `SessionRole` and the two new fields to `AgentSession`

**Files:**
- Modify: `crates/hermes-agent/src/session.rs:8-18,60-76,82-95,114-134,233-241`
- Test: `crates/hermes-agent/src/session.rs` (extend existing `mod tests`)

- [ ] **Step 1: Write the failing test for backward-compatible snapshot deserialization**

Add to the existing `mod tests` at the bottom of `crates/hermes-agent/src/session.rs`:

```rust
    #[tokio::test]
    async fn snapshot_round_trips_with_missing_sub_agent_fields() {
        // A snapshot written before this change has no parent_session_id or role field.
        let raw = br#"{
            "session_id": "legacy",
            "working_dir": "/tmp",
            "system_message": null,
            "messages": [],
            "context_usage_baseline_tokens": null
        }"#;
        let snapshot: SessionSnapshot = serde_json::from_slice(raw).unwrap();
        assert_eq!(snapshot.parent_session_id, None);
        assert_eq!(snapshot.role, SessionRole::Root);
    }

    #[tokio::test]
    async fn session_role_default_is_root() {
        assert_eq!(SessionRole::default(), SessionRole::Root);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p perry-hermes-agent --lib session::tests::snapshot_round_trips_with_missing_sub_agent_fields -- --nocapture`
Expected: compile error: `cannot find type SessionRole in this scope` / `no field parent_session_id on SessionSnapshot`.

- [ ] **Step 3: Add `SessionRole` and the new fields to the schema**

In `crates/hermes-agent/src/session.rs`:

1. Add a new import for `Default` derive support. The file already uses `serde::{Deserialize, Serialize}`; extend with `Default`:

   ```rust
   use serde::{Deserialize, Serialize};
   ```

   Add to the import line:

   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
   pub enum SessionRole {
       #[default]
       Root,
       SubAgent,
   }
   ```

2. Update the `SessionSnapshot` struct (currently `struct SessionSnapshot { ... }` at line 9) to:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   struct SessionSnapshot {
       session_id: String,
       working_dir: PathBuf,
       system_message: Option<Message>,
       messages: Vec<Message>,
       /// First provider-reported prompt context usage for this session.
       /// This is used after compaction to estimate:
       /// `baseline + summary_output_tokens`.
       #[serde(default)]
       context_usage_baseline_tokens: Option<u64>,
       /// Parent session id, if this is a sub-agent session.
       #[serde(default)]
       parent_session_id: Option<String>,
       /// Root (user-facing) or SubAgent. Defaults to Root for legacy snapshots.
       #[serde(default)]
       role: SessionRole,
   }
   ```

3. Update the `AgentSession` struct (currently at line 60) to:

   ```rust
   #[derive(Debug, Clone)]
   pub struct AgentSession {
       pub session_id: Arc<str>,
       pub working_dir: Arc<PathBuf>,
       pub system_message: Option<Arc<Message>>,
       pub parent_session_id: Option<Arc<str>>,
       pub role: SessionRole,
       messages: Arc<RwLock<Vec<Message>>>,
       /// First non-zero provider-reported prompt context usage observed for
       /// this session. In the initial turn, this usually includes the system
       /// prompt and the first user message. For providers with prompt caching,
       /// this stores normalized context occupancy (`input_tokens +
       /// cached_input_tokens`), not billing-only input tokens.
       ///
       /// After compaction, the best immediate context estimate is:
       /// `context_usage_baseline_tokens + summary_output_tokens`.
       context_usage_baseline_tokens: Arc<RwLock<Option<u64>>>,
       store: Option<JsonFileSessionStore>,
   }
   ```

4. Update `AgentSession::new` (currently at line 82) to initialize the new fields:

   ```rust
       pub fn new(
           session_id: impl Into<String>,
           working_dir: impl Into<PathBuf>,
           system_message: Option<Message>,
       ) -> Self {
           Self {
               session_id: Arc::from(session_id.into()),
               working_dir: Arc::new(working_dir.into()),
               system_message: system_message.map(Arc::new),
               parent_session_id: None,
               role: SessionRole::Root,
               messages: Arc::new(RwLock::new(Vec::with_capacity(8))),
               context_usage_baseline_tokens: Arc::new(RwLock::new(None)),
               store: None,
           }
       }
   ```

5. Update `load_json_file_with_system_message` (currently at line 114) to populate the new fields from the snapshot:

   ```rust
       pub(crate) async fn load_json_file_with_system_message(
           path: impl Into<PathBuf>,
           working_dir: Option<PathBuf>,
           system_message: Option<Message>,
       ) -> std::io::Result<Self> {
           let path = path.into();
           let raw = tokio::fs::read(&path).await?;
           let snapshot: SessionSnapshot =
               serde_json::from_slice(&raw).map_err(std::io::Error::other)?;
           let working_dir = working_dir.unwrap_or(snapshot.working_dir);
           Ok(Self {
               session_id: Arc::from(snapshot.session_id),
               working_dir: Arc::new(working_dir),
               system_message: system_message.or(snapshot.system_message).map(Arc::new),
               parent_session_id: snapshot.parent_session_id.map(Arc::from),
               role: snapshot.role,
               messages: Arc::new(RwLock::new(snapshot.messages)),
               context_usage_baseline_tokens: Arc::new(RwLock::new(
                   snapshot.context_usage_baseline_tokens,
               )),
               store: Some(JsonFileSessionStore::new(path)),
           })
       }
   ```

6. Update `snapshot()` (currently at line 233) to write the new fields:

   ```rust
       async fn snapshot(&self) -> SessionSnapshot {
           SessionSnapshot {
               session_id: self.session_id.to_string(),
               working_dir: self.working_dir.as_ref().clone(),
               system_message: self.system_message.as_deref().cloned(),
               messages: self.messages().await,
               context_usage_baseline_tokens: *self.context_usage_baseline_tokens.read().await,
               parent_session_id: self.parent_session_id.as_deref().map(str::to_string),
               role: self.role,
           }
       }
   ```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p perry-hermes-agent --lib session::tests -- --nocapture`
Expected: all session tests pass, including the two new ones.

- [ ] **Step 5: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-agent/src/session.rs
git commit -m "feat(agent): add SessionRole and sub-agent fields to AgentSession"
```

---

## Task 2: Add `archive_to` method to `AgentSession`

**Files:**
- Modify: `crates/hermes-agent/src/session.rs:213-241`
- Test: `crates/hermes-agent/src/session.rs` (extend existing `mod tests`)

- [ ] **Step 1: Write the failing test for `archive_to`**

Add to `mod tests`:

```rust
    #[tokio::test]
    async fn archive_to_moves_file_to_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let active_path = tmp.path().join("sessions").join("k.json");
        let session = AgentSession::new("k", PathBuf::from("/tmp/p"), None)
            .with_json_file_store(active_path.clone());
        session.append_message(Message::user("hello")).await;

        let archive_dir = tmp.path().join("archive");
        let archived = session.archive_to(&archive_dir).await.unwrap();

        assert!(archived.starts_with(archive_dir.join("k")));
        assert!(archived.exists(), "archive file should exist at {archived:?}");
        assert!(!active_path.exists(), "active file should be gone");
        assert!(
            session.messages().await.is_empty(),
            "in-memory messages should be cleared"
        );
    }

    #[tokio::test]
    async fn archive_to_with_no_store_returns_ok_with_no_filesystem_effect() {
        let session = AgentSession::new("k", PathBuf::from("/tmp/p"), None);
        let tmp = tempfile::tempdir().unwrap();
        let result = session.archive_to(tmp.path()).await;
        assert!(result.is_ok());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p perry-hermes-agent --lib session::tests::archive_to -- --nocapture`
Expected: compile error: `no method named archive_to found for struct AgentSession`.

- [ ] **Step 3: Implement `archive_to`**

In `crates/hermes-agent/src/session.rs`, add the following method to the `impl AgentSession` block (place it after `save_snapshot`, before `snapshot`):

```rust
       /// Move the current on-disk snapshot to `dir/<session_id>/<utc_ts>.json`
       /// and clear the in-memory history. The session retains its
       /// `session_id` and remains usable; the next `append_message`
       /// will recreate the file at the active path.
       ///
       /// If no `store` is attached, returns `Ok` with a path that
       /// was not written (used by in-memory tests and CLI startup
       /// paths that have not yet persisted).
       pub async fn archive_to(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
           let Some(store) = &self.store else {
               let placeholder = dir
                   .join(self.session_id.as_ref())
                   .join("no-store.json");
               return Ok(placeholder);
           };

           let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
           let target_dir = dir.join(self.session_id.as_ref());
           tokio::fs::create_dir_all(&target_dir).await?;
           let target = target_dir.join(format!("{ts}.json"));

           if store.path.as_ref().exists() {
               tokio::fs::rename(store.path.as_ref(), &target).await?;
           } else {
               // Nothing on disk yet; write a snapshot of the current
               // (possibly empty) state so the archive layout is
               // uniform across runs.
               let bytes = serde_json::to_vec_pretty(&self.snapshot().await)
                   .map_err(std::io::Error::other)?;
               tokio::fs::write(&target, bytes).await?;
           }

           // Clear in-memory messages. The active file no longer
           // exists; the next `append_message` will recreate it via
           // the existing `persist` path.
           self.clear_messages().await;
           self.reset_token_facts().await;
           Ok(target)
       }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p perry-hermes-agent --lib session::tests::archive_to -- --nocapture`
Expected: both new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-agent/src/session.rs
git commit -m "feat(agent): add AgentSession::archive_to for archival lifecycle"
```

---

## Task 3: Wire `load_json_file` into `SessionRegistry::get_or_create` with corrupt recovery

**Files:**
- Modify: `crates/hermes-agent/src/session_registry.rs:56-79,1-7,109-124`
- Test: `crates/hermes-agent/src/session_registry.rs` (extend existing `mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` at the bottom of `crates/hermes-agent/src/session_registry.rs`:

```rust
    #[tokio::test]
    async fn get_or_create_loads_existing_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        // Pre-populate a session file at the deterministic path.
        let key = "telegram:dm:123";
        let session_id = super::format_session_id_for_test(key);
        let snapshot = serde_json::json!({
            "session_id": session_id,
            "working_dir": "/tmp/old",
            "system_message": null,
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" }
            ],
            "context_usage_baseline_tokens": null,
        });
        let path = sessions.join(format!("{session_id}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();

        let registry = super::SessionRegistry::new(sessions, tmp.path().into(), None);
        let entry = registry.get_or_create(key).await;
        let messages = entry.session.messages().await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_text(), "hi");
        assert_eq!(messages[1].content.as_text(), "hello");
    }

    #[tokio::test]
    async fn get_or_create_recovers_from_corrupt_json() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let key = "telegram:dm:123";
        let session_id = super::format_session_id_for_test(key);
        let path = sessions.join(format!("{session_id}.json"));
        std::fs::write(&path, b"not json").unwrap();

        let registry = super::SessionRegistry::new(sessions.clone(), tmp.path().into(), None);
        let entry = registry.get_or_create(key).await;
        assert!(entry.session.messages().await.is_empty());

        // Original file is gone; a .corrupt-<ts>.json sibling exists.
        assert!(!path.exists(), "corrupt active file should be moved aside");
        let archive_dir = sessions.join(".archive").join(&session_id);
        let entries: Vec<_> = std::fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".corrupt.json")
            })
            .collect();
        assert_eq!(entries.len(), 1, "one .corrupt.json archive entry");
    }
```

To make `format_session_id` accessible to the test, expose a `pub(crate)` alias:

In `crates/hermes-agent/src/session_registry.rs`, change the `fn format_session_id` declaration (line 122) to:

```rust
pub(crate) fn format_session_id_for_test(key: &str) -> String {
    format_session_id(key)
}
```

Actually, a simpler path: change the visibility of `format_session_id` to `pub(crate)` directly:

```rust
pub(crate) fn format_session_id(key: &str) -> String {
    key.replace([':', '-'], "_")
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests::get_or_create_loads_existing_snapshot -- --nocapture`
Expected: fails — `get_or_create` constructs an empty session, so `messages.len() == 0` not 2.

- [ ] **Step 3: Add a timestamp helper**

At the top of `crates/hermes-agent/src/session_registry.rs`, extend the `use` line:

```rust
use chrono::{DateTime, Utc};
```

Add a new helper function near `format_session_id`:

```rust
/// Format a UTC timestamp suffix used for archive file names.
pub(crate) fn archive_timestamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string()
}
```

- [ ] **Step 4: Modify `get_or_create` to load from existing file**

Replace the body of `get_or_create` (currently at line 56) with:

```rust
       pub async fn get_or_create(&self, key: &str) -> Arc<SessionEntry> {
           if let Some(entry) = self.sessions.get(key) {
               *entry.last_active.lock().unwrap() = Utc::now();
               return Arc::clone(&entry);
           }

           let session_id = format_session_id(key);
           let store_path = self.sessions_dir.join(format!("{session_id}.json"));
           let system_message = self.system_message.clone();

           let session = if store_path.exists() {
               match AgentSession::load_json_file_with_system_message(
                   &store_path,
                   Some(self.working_dir.clone()),
                   system_message.clone(),
               )
               .await
               {
                   Ok(loaded) => loaded,
                   Err(err) => {
                       tracing::warn!(
                           error = %err,
                           path = %store_path.display(),
                           "failed to load existing session; archiving as corrupt"
                       );
                       let archive_dir = self
                           .sessions_dir
                           .join(".archive")
                           .join(&session_id);
                       if let Err(create_err) =
                           tokio::fs::create_dir_all(&archive_dir).await
                       {
                           tracing::warn!(
                               error = %create_err,
                               dir = %archive_dir.display(),
                               "could not create corrupt-archive dir"
                           );
                       } else {
                           let target = archive_dir
                               .join(format!("{}.corrupt.json", archive_timestamp()));
                           if let Err(rename_err) =
                               tokio::fs::rename(&store_path, &target).await
                           {
                               tracing::warn!(
                                   error = %rename_err,
                                   from = %store_path.display(),
                                   to = %target.display(),
                                   "could not move corrupt session aside"
                               );
                           }
                       }
                       AgentSession::new(
                           &session_id,
                           &self.working_dir,
                           system_message,
                       )
                       .with_json_file_store(&store_path)
                   }
               }
           } else {
               AgentSession::new(&session_id, &self.working_dir, system_message)
                   .with_json_file_store(&store_path)
           };

           let now = Utc::now();
           let entry = Arc::new(SessionEntry {
               session,
               turn_lock: Mutex::new(()),
               created_at: now,
               last_active: std::sync::Mutex::new(now),
           });

           self.sessions.insert(key.to_string(), Arc::clone(&entry));
           entry
       }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests -- --nocapture`
Expected: all four tests in this file pass (the two new ones plus the two existing ones).

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-agent/src/session_registry.rs
git commit -m "fix(agent): load existing session snapshot on get_or_create"
```

---

## Task 4: Add `archive_active` to `SessionRegistry`

**Files:**
- Modify: `crates/hermes-agent/src/session_registry.rs:81-91`
- Test: `crates/hermes-agent/src/session_registry.rs` (extend existing `mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[tokio::test]
    async fn archive_active_moves_file_and_clears_in_memory_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let registry = super::SessionRegistry::new(
            sessions.clone(),
            tmp.path().into(),
            None,
        );
        let entry = registry.get_or_create("k").await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("hi"))
            .await;

        let archived = registry.archive_active("k").await;
        assert!(archived.is_some(), "archive_active should return a path");
        let archived = archived.unwrap();
        assert!(archived.exists(), "archive file should exist");
        assert!(entry.session.messages().await.is_empty());

        // Re-getting the same key after archive starts a fresh session.
        let entry2 = registry.get_or_create("k").await;
        assert!(entry2.session.messages().await.is_empty());
    }

    #[tokio::test]
    async fn archive_active_returns_none_for_missing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = super::SessionRegistry::new(
            tmp.path().join("sessions"),
            tmp.path().into(),
            None,
        );
        assert!(registry.archive_active("nope").await.is_none());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests::archive_active -- --nocapture`
Expected: compile error: `no method named archive_active`.

- [ ] **Step 3: Implement `archive_active`**

Add the method to `impl SessionRegistry` (right after `reset`):

```rust
       /// Archive the active on-disk snapshot for `key` to
       /// `sessions/.archive/<key>/<utc_ts>.json`. Returns `None` if
       /// `key` has no live session.
       pub async fn archive_active(&self, key: &str) -> Option<PathBuf> {
           let entry = self.sessions.get(key)?.clone();
           let _guard = entry.turn_lock.lock().await;
           let archive_dir = self.sessions_dir.join(".archive");
           match entry.session.archive_to(&archive_dir).await {
               Ok(path) => Some(path),
               Err(err) => {
                   tracing::warn!(
                       error = %err,
                       session = %key,
                       "failed to archive active session"
                   );
                   None
               }
           }
       }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests::archive_active -- --nocapture`
Expected: both new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-agent/src/session_registry.rs
git commit -m "feat(agent): add SessionRegistry::archive_active"
```

---

## Task 5: Update `SessionRegistry::reset` to archive first

**Files:**
- Modify: `crates/hermes-agent/src/session_registry.rs:81-91`
- Test: `crates/hermes-agent/src/session_registry.rs` (extend existing `mod tests`)

- [ ] **Step 1: Update the existing `reset_clears_messages` test**

In `crates/hermes-agent/src/session_registry.rs`, the existing test at line 148 currently asserts `reset` clears messages. Augment it (or add a new test alongside it) to assert the archive side effect:

```rust
    #[tokio::test]
    async fn reset_archives_then_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let registry = super::SessionRegistry::new(
            sessions.clone(),
            tmp.path().into(),
            None,
        );
        let entry = registry.get_or_create("k").await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("hi"))
            .await;

        assert!(registry.reset("k").await);
        assert!(entry.session.messages().await.is_empty());

        // The prior content moved to .archive/k/<ts>.json.
        let archive_dir = sessions.join(".archive").join("k");
        assert!(archive_dir.exists(), "archive dir should be created");
        let count = std::fs::read_dir(&archive_dir).unwrap().count();
        assert_eq!(count, 1, "one archive entry expected");
    }
```

Leave the existing `reset_clears_messages` test as-is — both can coexist.

- [ ] **Step 2: Modify `reset` to archive first**

Replace the `reset` method body (currently at line 83) with:

```rust
       pub async fn reset(&self, key: &str) -> bool {
           let Some(entry) = self.sessions.get(key) else {
               return false;
           };
           let entry = entry.clone();
           let _guard = entry.turn_lock.lock().await;

           // Best-effort archive. Failure is logged inside `archive_active`.
           let archive_dir = self.sessions_dir.join(".archive");
           if let Err(err) = entry.session.archive_to(&archive_dir).await {
               tracing::warn!(
                   error = %err,
                   session = %key,
                   "reset could not archive active session; clearing in place"
               );
           }
           entry.session.reset().await;
           *entry.last_active.lock().unwrap() = Utc::now();
           true
       }
```

Note: `entry.session.reset()` is the existing in-memory clear — leave it.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests -- --nocapture`
Expected: all five tests pass (`reset_clears_messages`, `reset_archives_then_clears`, `reset_nonexistent_returns_false`, plus the four from Tasks 3 and 4).

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-agent/src/session_registry.rs
git commit -m "feat(agent): reset archives the active session before clearing"
```

---

## Task 6: Add `create_sub_session` to `SessionRegistry`

**Files:**
- Modify: `crates/hermes-agent/src/session_registry.rs:81-107`
- Test: `crates/hermes-agent/src/session_registry.rs` (extend existing `mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[tokio::test]
    async fn create_sub_session_sets_role_and_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let registry = super::SessionRegistry::new(
            sessions.clone(),
            tmp.path().into(),
            None,
        );
        let parent = registry.get_or_create("parent_key").await;

        let child = registry
            .create_sub_session("parent_key", "sub-1")
            .await;

        use perry_hermes_agent::session::SessionRole;
        assert_eq!(child.session.role, SessionRole::SubAgent);
        assert_eq!(
            child.session.parent_session_id.as_deref(),
            Some("parent_key")
        );
        // Distinct from the parent.
        assert_ne!(child.session.session_id.as_ref(), parent.session.session_id.as_ref());
    }
```

Add the import at the top of the file if not already present:

```rust
use crate::session::SessionRole;
```

(The line `use crate::session::AgentSession;` already exists — extend that import.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests::create_sub_session -- --nocapture`
Expected: compile error: `no method named create_sub_session`.

- [ ] **Step 3: Implement `create_sub_session`**

Add the method to `impl SessionRegistry` (after `archive_active`):

```rust
       /// Reserve a sub-agent session for the given parent. The
       /// child key is derived as
       /// `<parent_key>__sub_<sub_id>__<utc_ts>`. The child is
       /// persisted with `role = SubAgent` and
       /// `parent_session_id = parent_key`.
       ///
       /// This method is reserved for the future sub-agent runtime
       /// and is not invoked by any adapter today.
       pub async fn create_sub_session(
           &self,
           parent_key: &str,
           sub_id: &str,
       ) -> Arc<SessionEntry> {
           let ts = archive_timestamp();
           let child_key = format!("{parent_key}__sub_{sub_id}__{ts}");
           let entry = self.get_or_create(&child_key).await;

           // Construct a patched session that shares the message log
           // and store (both held by Arc inside AgentSession) with
           // the entry returned above, but with the sub-agent
           // identity stamped on.
           let patched = AgentSession {
               parent_session_id: Some(Arc::from(parent_key)),
               role: crate::session::SessionRole::SubAgent,
               ..(*entry.session).clone()
           };
           // Persist immediately so the on-disk snapshot reflects
           // the new identity even if the caller never appends a
           // message.
           let _ = patched.save().await;

           // The first `get_or_create` produced an entry with an
           // identity-less AgentSession; replace it with the patched
           // one. The fresh `turn_lock` and `created_at` are
           // intentional — a sub-agent runs independently of the
           // parent.
           let now = Utc::now();
           let new_entry = Arc::new(SessionEntry {
               session: patched,
               turn_lock: Mutex::new(()),
               created_at: now,
               last_active: std::sync::Mutex::new(now),
           });
           self.sessions.insert(child_key, new_entry.clone());
           new_entry
       }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests::create_sub_session -- --nocapture`
Expected: test passes.

- [ ] **Step 5: Run the full session_registry test suite**

Run: `cargo test -p perry-hermes-agent --lib session_registry::tests -- --nocapture`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/hermes-agent/src/session_registry.rs
git commit -m "feat(agent): add SessionRegistry::create_sub_session for sub-agents"
```

---

## Task 7: Update gateway runner `/reset` and `/status`

**Files:**
- Modify: `crates/hermes-gateway/src/runner.rs:111-128,240-252`

- [ ] **Step 1: Verify the `/reset` branch in `handle_event`**

In `crates/hermes-gateway/src/runner.rs`, the existing branch (around line 113) is:

```rust
                Command::Reset | Command::New => {
                    let key = build_key(&event);
                    self.sessions.reset(&key).await;
                    info!(session = %key, "session reset by user");
                    Ok(GatewayResponse::Reply("Session has been reset.".into()))
                }
```

`reset` already archives internally (Task 5), so this branch needs no change. The `tracing::warn!` calls inside `reset` and `archive_active` surface archive failures. No edit required for this step — confirm by reading the file and proceeding.

- [ ] **Step 2: Update `handle_status` to show the archive count**

In `crates/hermes-gateway/src/runner.rs`, replace the `handle_status` method (currently at line 240) with:

```rust
       async fn handle_status(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
           let entry = self.session(event).await;
           let messages = entry.session.messages().await;
           let key = build_key(event);
           let session_id = key.replace([':', '-'], "_");
           let archive_dir = self
               .config
               .sessions_dir
               .join(".archive")
               .join(&session_id);
           let archived = count_files_in(&archive_dir).await;

           Ok(GatewayResponse::Reply(format!(
               "Session: {}\nMessages: {}\nWorking dir: {}\nArchived: {}",
               key,
               messages.len(),
               entry.session.working_dir.display(),
               archived,
           )))
       }
```

Add a small helper at the bottom of the file (outside the `impl`):

```rust
async fn count_files_in(dir: &std::path::Path) -> u64 {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    let mut n = 0u64;
    while let Ok(Some(_)) = rd.next_entry().await {
        n += 1;
    }
    n
}
```

- [ ] **Step 3: Verify the gateway crate builds**

Run: `cargo build -p perry-hermes-gateway`
Expected: success.

- [ ] **Step 4: Run existing gateway tests**

Run: `cargo test -p perry-hermes-gateway --no-run`
Expected: compiles. (There are no runtime tests for the runner today; this step confirms no regression.)

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-gateway/src/runner.rs
git commit -m "feat(gateway): /status reports archive count"
```

---

## Task 8: CLI archives its session on clean shutdown

**Files:**
- Modify: `crates/hermes-cli/src/tui/run.rs:115-183`

- [ ] **Step 1: Capture `cli_key` and call `archive_active` after the run loop**

In `crates/hermes-cli/src/tui/run.rs`, replace the section from line 119 to line 182 (the run loop and the `disable_raw_mode` postlude) with:

```rust
    let cli_key = new_cli_session_key();
    let entry = registry.get_or_create(&cli_key).await;
    let session = entry.session.clone();

    let result: Result<(), RunError> = async {
        loop {
            draw_inline_bottom(&mut terminal, &mut app, &mut history)?;

            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let width = app.history_width;
                    history.push(&mut app, RenderedLine::System("⚠ cancelled".to_string()), width);
                    draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                    return Ok(());
                }
                _ = tick.tick() => {
                    // Periodic redraw keeps the display fresh while streaming.
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(k))) => {
                            let next = handle_key(&mut app, k);
                            if dispatch_event(
                                &mut app,
                                next,
                                &cancel,
                                Some(RunContext {
                                    agent: &agent,
                                    session: &session,
                                    input_tx: &input_tx,
                                }),
                                Some(&mut history),
                            )? {
                                draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                                return Ok(());
                            }
                        }
                        Some(Ok(Event::Resize(_, _))) => {
                            // Next tick will redraw at the new size.
                        }
                        Some(Err(e)) => {
                            return Err(RunError::Tui(e.to_string()));
                        }
                        None => return Ok(()),
                        _ => {}
                    }
                }
                maybe = input_rx.recv() => {
                    if let Some(ev) = maybe
                        && dispatch_event(&mut app, ev, &cancel, None, Some(&mut history))? {
                            draw_inline_bottom(&mut terminal, &mut app, &mut history)?;
                            return Ok(());
                        }
                }
            }
        }
    }
    .await;

    if let Err(e) = disable_raw_mode() {
        eprintln!("[perry-hermes] warning: failed to disable raw mode: {e}");
    }

    // Best-effort archive of this CLI run's session. Failure is
    // logged inside `archive_active` and does not affect the run
    // result returned to the caller.
    let _ = registry.archive_active(&cli_key).await;

    result
}
```

The single-line capture `let cli_key = new_cli_session_key();` plus the postlude `let _ = registry.archive_active(&cli_key).await;` are the only additions. The rest is unchanged.

- [ ] **Step 2: Verify the CLI crate builds**

Run: `cargo build -p perry-hermes-cli`
Expected: success.

- [ ] **Step 3: Run existing CLI tests**

Run: `cargo test -p perry-hermes-cli tui -- --nocapture`
Expected: existing TUI tests pass; the new archive call is a no-op for tests that do not assert filesystem state.

- [ ] **Step 4: Commit**

```bash
git add crates/hermes-cli/src/tui/run.rs
git commit -m "feat(cli): archive active session on clean shutdown"
```

---

## Task 9: Integration test for gateway restart continuity

**Files:**
- Create: `crates/hermes-gateway/tests/session_lifecycle.rs`

- [ ] **Step 1: Create the integration test file**

Create `crates/hermes-gateway/tests/session_lifecycle.rs`:

```rust
//! Verifies that an `AgentSession` produced under one `SessionRegistry`
//! is loaded back by a fresh registry built with the same sessions
//! directory.

use std::path::PathBuf;
use std::sync::Arc;

use perry_hermes_agent::{AgentSession, SessionRegistry};

#[tokio::test]
async fn registry_restart_loads_existing_history() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let key = "telegram:dm:42";

    // First "process": create the session and append some messages.
    {
        let registry = SessionRegistry::new(
            sessions_dir.clone(),
            PathBuf::from("/tmp/project"),
            None,
        );
        let entry = registry.get_or_create(key).await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("remember this"))
            .await;
        // Simulate a process exit: archive the active session.
        let _ = registry.archive_active(key).await;
    }

    // The archive should now contain one entry.
    let session_id = key.replace([':', '-'], "_");
    let archive_dir = sessions_dir.join(".archive").join(&session_id);
    let archived_count = std::fs::read_dir(&archive_dir).unwrap().count();
    assert_eq!(archived_count, 1, "one archive entry expected");

    // Second "process": rebuild the registry, ask for the same key,
    // and confirm the history is loaded from disk.
    {
        let registry = SessionRegistry::new(
            sessions_dir.clone(),
            PathBuf::from("/tmp/project"),
            None,
        );
        let entry = registry.get_or_create(key).await;
        let messages = entry.session.messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_text(), "remember this");
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p perry-hermes-gateway --test session_lifecycle -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 4: Run clippy to catch lints introduced by the new code**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/hermes-gateway/tests/session_lifecycle.rs
git commit -m "test(gateway): integration test for session restart continuity"
```

---

## Final Verification

- [ ] **Step 1: Format, build, test, lint, doc**

Run:

```bash
cargo fmt --all
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo doc --no-deps
```

Expected: every step succeeds with no warnings.

- [ ] **Step 2: Manual smoke test**

1. `cargo run -p perry-hermes-cli` — type a few messages, exit (Ctrl+C).
2. Verify `~/.perry_hermes/sessions/` no longer contains the per-run file; `~/.perry_hermes/sessions/.archive/cli_run_<id>/<ts>.json` does.
3. `cargo run -p perry-hermes-cli` again — type "/status". Expect to see `Messages: 0` (the new run) and `Archived: 1` (the previous run).

- [ ] **Step 3: Tag the change**

```bash
git tag -a v0.4.3 -m "v0.4.3: gateway session lifecycle"
git push origin feature/session-management
```
