# Gateway Session Lifecycle & Persistence Design

## Goal

Fix the data-loss bug where restarting the gateway wipes the conversation
history of a Telegram or QQ chat, and replace the ad-hoc file behavior
with a proper session lifecycle that supports archival, restoration,
and forward-compatible sub-agent contexts.

Concretely:

- `telegram_dm_674971091.json` (and every other `sessions/<key>.json`)
  must load on gateway startup, not be overwritten with empty content.
- `/reset` must not silently destroy history; the pre-reset content
  must be retrievable.
- CLI runs must leave the `sessions/` folder as clean as gateway runs.
- The schema must leave room for sub-agent sessions and a future
  `/resume` command without a follow-up migration.

## Scope

**In scope:**

- Wire `AgentSession::load_json_file_with_system_message` into
  `SessionRegistry::get_or_create`.
- Define the active vs. archive storage layout and the lifecycle that
  moves between them.
- Extend `SessionRegistry` with `archive_active` and
  `create_sub_session`.
- Extend `AgentSession` with `parent_session_id` and `SessionRole`
  so sub-agent sessions can land later without a migration.
- Update `/reset`, the gateway runner, and the CLI shutdown path to
  archive the active file.
- Define corrupt-JSON recovery semantics.

**Out of scope (future iterations):**

- Implementing the `/resume` command. The registry API is reserved
  in this change; the parser variant ships later.
- Sub-agent runtime behavior. Only the storage layer is wired.
- A UI for browsing archives (`/archive`). The runner's `/status`
  command reports an archive count.
- Pruning or rotating old archives. They accumulate.
- Migrating existing on-disk sessions that lack
  `parent_session_id` / `role` in their JSON. Those default to
  `Root` / `None` on read.

## Root Cause

`SessionRegistry::get_or_create`
([`crates/hermes-agent/src/session_registry.rs:56`](../../crates/hermes-agent/src/session_registry.rs))
always constructs a fresh `AgentSession` with
`AgentSession::new(...).with_json_file_store(...)` and never calls
`AgentSession::load_json_file_with_system_message`. On restart, a
new in-memory session is wired to the existing JSON path; the first
`append_message` writes `{messages: [user]}` on top of the prior
content. The persisted file *is* the load target — it is just never
loaded.

`AgentSession::load_json_file`
([`crates/hermes-agent/src/session.rs:110`](../../crates/hermes-agent/src/session.rs))
and `AIAgent::load_json_session`
([`crates/hermes-agent/src/runtime_agent.rs:89`](../../crates/hermes-agent/src/runtime_agent.rs))
exist and are tested. The fix is wiring, not new code.

## Decisions

| Question | Decision | Rationale |
|---|---|---|
| Restart default | **Load full history** | Matches user expectation that a chat remembers its prior turns. |
| History cap | **No cap, rely on context compaction** | `AgentSession` already triggers `SummaryCompactor` when the provider reports context usage at or above `context_compression_threshold_percent * context_window_size`. Adding a message-count cap would compete with the compactor. |
| Corrupt JSON | **Backup as `.corrupt-<ts>.json`, then start empty** | Preserves evidence, doesn't block the gateway. |
| Multi-user in one chat | **Shared session** | The current `build_key` does not include `user_id`; this design keeps that. Group conversations stay shared. |
| `/reset` | **Archive prior content, then start empty** | One operation, atomic from the user's perspective; the prior conversation is recoverable. |
| Storage layout | **`sessions/` for active, `sessions/.archive/<key>/<ts>.json` for archives** | Mirrors the existing `~/.hermes/skills/.archive/` pattern. Keeps the live folder clean. |
| CLI exit | **Archive on clean shutdown** | Same end state as `/reset`: an empty active file (or none) plus an archive entry. |
| `/resume` | **Reserve the API, defer the command** | The registry method is added now so the parser work in a follow-up is purely additive. |
| Sub-agent storage | **Same `JsonFileSessionStore`, naming `<parent_key>__sub_<sub_id>__<utc_ts>.json`, fields `parent_session_id` + `SessionRole` on the session** | Reuses the persistence path. Old snapshots deserialize as `Root` / `None` for forward compatibility. |
| Crash mid-archive | **Acceptable to leave old snapshot in archive and new file empty** | Not destructive. The archive is the source of truth. |

## Storage Layout

```
~/.perry_hermes/
├── sessions/                          # active sessions only
│   ├── telegram_dm_674971091.json
│   ├── qqbot_dm_<id>.json
│   └── cli_<pid>_<run_n>.json
└── sessions/.archive/                 # archived snapshots
    └── <key>/
        ├── 20260609T120000000Z.json   # from /reset
        ├── 20260609T130000000Z.json   # from CLI exit
        └── 20260609T140000000Z.corrupt.json
```

- `<key>` reuses `format_session_id` (e.g. `telegram_dm_674971091`).
- `<utc_timestamp>` is `chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ")`.
- A `<key>` has at most one active file.
- Archives are append-only; nothing in the runtime prunes them.

## Lifecycle

| Event | Active file | Archive |
|---|---|---|
| First message for new key | create `<key>.json` with empty messages | — |
| Restart (active file exists) | **load** it into memory | — |
| Restart (no file) | create empty | — |
| Restart (active file is corrupt) | rename bad file → `.corrupt-<ts>.json`; create empty | the corrupt file is the archive |
| `/reset` | overwrite with empty messages | move prior contents to `.archive/<key>/<ts>.json` |
| Clean CLI process exit | active file is moved out (no longer present at the active path) | move prior contents to `.archive/<key>/<ts>.json` |
| Sub-agent turn (future) | create `<parent_key>__sub_<sub_id>__<utc_ts>.json` with empty messages | sub-agents have their own archive lifecycle, independent of their parent |

The `key` of a sub-agent is a deterministic function of its parent
key and a runtime-assigned `n`, so a re-derived key never collides
with a parent's archive.

## API Changes

### `crates/hermes-agent/src/session.rs`

- New `pub enum SessionRole { Root, SubAgent }` with `Default = Root`.
- New fields on `AgentSession`:
  - `pub parent_session_id: Option<Arc<str>>`
  - `pub role: SessionRole`
- `SessionSnapshot` gains the same two fields. Existing on-disk JSON
  files lacking the fields deserialize via `serde(default)` to
  `None` and `Root` — no migration.
- New method:

  ```rust
  impl AgentSession {
      /// Move the current on-disk snapshot to `dir/<key>/<utc_ts>.json`
      /// and clear the in-memory history. The session retains its
      /// `session_id` and remains usable; the next `append_message`
      /// will recreate the file at the active path.
      pub async fn archive_to(&self, dir: &Path) -> std::io::Result<PathBuf>;
  }
  ```

  Implementation notes:
  - If no `store` is attached, returns `Ok` with a sentinel path and
    no filesystem effect (test/CLI-in-memory path).
  - If the file does not exist on disk (session was never persisted),
    creates an empty archive entry to keep the layout consistent.
  - The `messages` vector is cleared *after* the file move so a
    mid-archive crash leaves the archive complete and the active
    file untouched.

### `crates/hermes-agent/src/session_registry.rs`

- `get_or_create(key)`:
  - If the in-memory cache has the key, return the existing entry
    (unchanged).
  - Else compute `store_path = sessions_dir / format!("{key}.json")`.
  - If `store_path` exists, call
    `AgentSession::load_json_file_with_system_message(store_path, Some(working_dir), system_message)`.
  - If load returns `Err`, log a `warn!` and rename the file to
    `sessions_dir/.archive/<key>/<utc_ts>.corrupt.json`. Then fall
    through to the empty-session construction.
  - Else construct an empty session, attach the same `store_path`,
    and return.
- `reset(key)`: now first calls `archive_active(key)` (best-effort,
  log on failure), then continues with the in-memory `reset()`. The
  public return type stays `bool` (whether a session existed).
- New methods:

  ```rust
  impl SessionRegistry {
      /// Archive the active file for `key` to `sessions/.archive/`.
      /// Returns `None` if the key has no live session.
      pub async fn archive_active(&self, key: &str) -> Option<PathBuf>;

      /// Reserve a sub-agent session for the given parent. Reserved
      /// for future sub-agent work; not called by any adapter today.
      pub async fn create_sub_session(
          &self,
          parent_key: &str,
          sub_id: &str,
      ) -> Arc<SessionEntry>;
  }
  ```

  `create_sub_session` builds the child key
  `<parent_key>__sub_<sub_id>__<utc_ts>`, calls `get_or_create` on
  it, and patches the resulting `AgentSession`'s
  `parent_session_id` and `role = SubAgent`. Persisting those fields
  happens automatically on the next `append_message`.

### `crates/hermes-gateway/src/runner.rs`

- `handle_event` `/reset` path now invokes
  `self.sessions.archive_active(&key).await` before the in-memory
  reset. If archiving returns `Err`, the runner replies with a
  warning line plus "Session has been reset." so the user still gets
  feedback.
- `handle_status` reports an additional line
  `Archived: N` from a directory scan of
  `sessions_dir/.archive/<key>/`.

### CLI shutdown

The CLI's run loop in
[`crates/hermes-cli/src/tui/run.rs`](../../crates/hermes-cli/src/tui/run.rs)
already captures `let cli_key = new_cli_session_key();` and
`let registry = SessionRegistry::new(...)` before the loop. After
the run loop returns, before the function returns to the caller, it
calls `registry.archive_active(&cli_key).await`. The CLI key
remains the existing `cli:run:<id>` shape (filename
`cli_run_<id>`); the per-run file goes to `.archive/cli_run_<id>/`.

## Concurrency

- `turn_lock` continues to serialize turns per session.
- `archive_active` takes the entry's `turn_lock` first, then moves
  the file, then clears messages. An in-flight turn cannot race a
  `/reset`.
- The archive move and the active truncate are sequenced within
  one critical section per session, but they are not atomic across
  the two filesystem operations. A crash between them leaves the
  archive complete and the active file unchanged — not destructive.
  The next startup will load the (unchanged) active file, which is
  the right behavior.

## Error Handling

| Failure | Behavior |
|---|---|
| Active file present but unparseable | Rename to `.corrupt-<ts>.json`, log warn, start empty. |
| Archive target dir not writable | `archive_active` returns `None` and logs a `tracing::warn!`; the gateway's `/reset` proceeds in memory and reports a warning in its reply. |
| `archive_to` called on a session with no `store` | Returns `Ok` with the would-be path; no filesystem effect. |
| Two parallel `archive_active` for the same key | The second sees the file already moved and the in-memory messages already cleared, and produces a follow-up empty archive entry. Acceptable. |
| Disk full during archive | Returns `Err`; the active file remains intact; user is told. |

## Testing

Unit tests in `crates/hermes-agent/src/session_registry.rs`:

- `get_or_create_loads_existing_snapshot`: write a populated session
  file, drop the registry, rebuild, assert messages reappear.
- `get_or_create_recovers_from_corrupt_json`: write garbage to the
  file, rebuild, assert the bad file is renamed with a
  `.corrupt-<ts>.json` suffix and the in-memory session is empty.
- `archive_active_moves_file_and_clears_in_memory_messages`:
  populate, archive, assert file moved and `messages().await` is
  empty.
- `reset_archives_then_clears`: same path as `archive_active` then
  in-memory clear.
- `create_sub_session_sets_role_and_parent`: child session has
  `role == SubAgent` and `parent_session_id == parent_key`.
- `snapshot_round_trips_with_missing_sub_agent_fields`: a snapshot
  written before this change deserializes with `role == Root` and
  `parent_session_id == None`.

Integration test in `crates/hermes-gateway/tests/`:

- Drive a `GatewayRunner` with a mock `PlatformAdapter`, send one
  user message, drop the runner, rebuild, send a second message
  that asks "what did I say last time?", and assert the agent's
  outbound history contains the first turn.

## Compatibility

- Existing JSON snapshots keep working: missing `parent_session_id`
  and `role` deserialize as `None` and `Root`.
- Existing CLI session filenames keep working; the shutdown
  archive is a one-time-on-exit action.
- `Command` enum is unchanged. `/resume` is not added in this
  change.
- `AgentSession`'s public API gains two fields and one method.
  Callers that destructure the struct will need an update; current
  callers in the workspace only use method-form access.
