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

    /// Get an existing session or create a new one.
    /// Returns an `Arc` so all callers share the same `turn_lock`.
    pub async fn get_or_create(&self, key: &str) -> Arc<SessionEntry> {
        if let Some(entry) = self.sessions.get(key) {
            *entry.last_active.lock().unwrap() = Utc::now();
            return Arc::clone(&entry);
        }

        let session_id = format_session_id(key);
        let store_path = self.sessions_dir.join(format!("{session_id}.json"));
        let system_message = self.system_message.clone();

        let session = AgentSession::new(&session_id, &self.working_dir, system_message)
            .with_json_file_store(&store_path);

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
fn format_session_id(key: &str) -> String {
    key.replace([':', '-'], "_")
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
}
