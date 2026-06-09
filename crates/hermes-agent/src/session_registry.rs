use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::Mutex;

use perry_hermes_core::Message;

use crate::session::AgentSession;

/// Concurrent session store keyed by arbitrary string keys.
///
/// Each entry holds an [`AgentSession`] and a per-session mutex that
/// serializes concurrent agent turns for the same conversation.
///
/// Used by both the CLI TUI (single session) and the Gateway
/// (multi-platform, multi-chat).
pub struct SessionRegistry {
    sessions: DashMap<String, Arc<SessionEntry>>,
    sessions_dir: PathBuf,
    working_dir: PathBuf,
    system_message: Option<Message>,
}

/// A managed session with concurrency control.
#[derive(Debug)]
pub struct SessionEntry {
    pub session: AgentSession,
    /// Serializes concurrent turns for this session.
    pub turn_lock: Mutex<()>,
    pub created_at: DateTime<Utc>,
    pub last_active: std::sync::Mutex<DateTime<Utc>>,
}

impl SessionRegistry {
    /// Create a new registry. Sessions are persisted as JSON files
    /// in `sessions_dir`. The `system_message`, if provided, is used
    /// as the immutable system prompt for every session created by
    /// this registry.
    pub fn new(
        sessions_dir: PathBuf,
        working_dir: PathBuf,
        system_message: Option<Message>,
    ) -> Self {
        Self {
            sessions: DashMap::new(),
            sessions_dir,
            working_dir,
            system_message,
        }
    }

    /// Get an existing session, load it from disk, or create a new
    /// one. The on-disk snapshot is at
    /// `sessions_dir/<format_session_id(key)>.json`; if it exists
    /// and is parseable, it is loaded. If it exists but is not
    /// parseable, the corrupt file is moved to
    /// `.archive/<key>/<ts>.corrupt.json` and a fresh empty
    /// session is constructed so the caller is not blocked.
    /// Returns an `Arc` so all callers share the same
    /// `turn_lock`.
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
                    let archive_dir = self.sessions_dir.join(".archive").join(&session_id);
                    if let Err(create_err) = tokio::fs::create_dir_all(&archive_dir).await {
                        tracing::warn!(
                        error = %create_err,
                        dir = %archive_dir.display(),
                        "could not create corrupt-archive dir"
                        );
                    } else {
                        let target =
                            archive_dir.join(format!("{}.corrupt.json", archive_timestamp()));
                        if let Err(rename_err) = tokio::fs::rename(&store_path, &target).await {
                            tracing::warn!(
                            error = %rename_err,
                            from = %store_path.display(),
                            to = %target.display(),
                            "could not move corrupt session aside"
                            );
                        }
                    }
                    AgentSession::new(&session_id, &self.working_dir, system_message)
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

    /// Reset the session for `key`, clearing message history.
    /// Returns false if no session exists.
    pub async fn reset(&self, key: &str) -> bool {
        if let Some(entry) = self.sessions.get(key) {
            entry.session.reset().await;
            *entry.last_active.lock().unwrap() = Utc::now();
            true
        } else {
            false
        }
    }

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

    /// Get a reference to the session for `key`, if it exists.
    pub fn get_session(&self, key: &str) -> Option<AgentSession> {
        self.sessions.get(key).map(|e| e.session.clone())
    }

    /// Returns the number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns true if there are no active sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Compute the default sessions directory.
pub fn default_sessions_dir() -> PathBuf {
    if let Ok(home) = std::env::var("PERRY_HERMES_HOME") {
        return PathBuf::from(home).join("sessions");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".perry_hermes").join("sessions");
    }
    PathBuf::from(".perry_hermes").join("sessions")
}

/// Format a session ID from a session key.
/// Replaces characters that are problematic in filenames.
pub(crate) fn format_session_id(key: &str) -> String {
    key.replace([':', '-'], "_")
}

/// Format a UTC timestamp suffix used for archive file names.
pub(crate) fn archive_timestamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_session_id_replaces_colons_and_dashes() {
        assert_eq!(format_session_id("telegram:dm:123"), "telegram_dm_123");
    }

    #[tokio::test]
    async fn get_or_create_returns_shared_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(tmp.path().into(), tmp.path().into(), None);

        let entry1 = registry.get_or_create("telegram:dm:123").await;
        let entry2 = registry.get_or_create("telegram:dm:123").await;

        assert_eq!(entry1.session.session_id, entry2.session.session_id);
        assert!(Arc::ptr_eq(&entry1, &entry2));
    }

    #[tokio::test]
    async fn reset_clears_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(tmp.path().into(), tmp.path().into(), None);

        let entry = registry.get_or_create("telegram:dm:123").await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("hello"))
            .await;
        assert!(!entry.session.messages().await.is_empty());

        assert!(registry.reset("telegram:dm:123").await);
        let session = registry.get_session("telegram:dm:123").unwrap();
        assert!(session.messages().await.is_empty());
    }

    #[tokio::test]
    async fn reset_nonexistent_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(tmp.path().into(), tmp.path().into(), None);
        assert!(!registry.reset("nonexistent").await);
    }

    #[tokio::test]
    async fn get_or_create_loads_existing_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        // Pre-populate a session file at the deterministic path.
        let key = "telegram:dm:123";
        let session_id = super::format_session_id(key);
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
    async fn get_or_create_loads_empty_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let key = "telegram:dm:empty";
        let session_id = super::format_session_id(key);
        let path = sessions.join(format!("{session_id}.json"));
        let snapshot = serde_json::json!({
        "session_id": session_id,
        "working_dir": "/tmp/old",
        "system_message": null,
        "messages": [],
        "context_usage_baseline_tokens": null,
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();

        let registry = super::SessionRegistry::new(sessions, tmp.path().into(), None);
        let entry = registry.get_or_create(key).await;
        assert!(entry.session.messages().await.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_loads_sub_agent_identity() {
        use crate::session::SessionRole;

        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let key = "telegram:dm:sub";
        let session_id = super::format_session_id(key);
        let path = sessions.join(format!("{session_id}.json"));
        let snapshot = serde_json::json!({
        "session_id": session_id,
        "working_dir": "/tmp/old",
        "system_message": null,
        "messages": [],
        "context_usage_baseline_tokens": null,
        "parent_session_id": "parent_key",
        "role": "SubAgent",
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();

        let registry = super::SessionRegistry::new(sessions, tmp.path().into(), None);
        let entry = registry.get_or_create(key).await;
        assert_eq!(entry.session.role, SessionRole::SubAgent);
        assert_eq!(
            entry.session.parent_session_id.as_deref(),
            Some("parent_key")
        );
    }

    #[tokio::test]
    async fn get_or_create_recovers_from_corrupt_json() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let key = "telegram:dm:123";
        let session_id = super::format_session_id(key);
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
            .filter(|e| e.file_name().to_string_lossy().ends_with(".corrupt.json"))
            .collect();
        assert_eq!(entries.len(), 1, "one .corrupt.json archive entry");
    }

    #[tokio::test]
    async fn archive_active_moves_file_and_clears_in_memory_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let registry = super::SessionRegistry::new(sessions.clone(), tmp.path().into(), None);
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
        let registry =
            super::SessionRegistry::new(tmp.path().join("sessions"), tmp.path().into(), None);
        assert!(registry.archive_active("nope").await.is_none());
    }
}
