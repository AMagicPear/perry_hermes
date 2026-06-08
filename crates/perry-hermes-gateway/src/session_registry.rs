use std::path::PathBuf;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::Mutex;

use perry_hermes_agent::AgentSession;

use crate::event::GatewayEvent;

/// Concurrent session store keyed by session key strings.
///
/// Each entry holds an [`AgentSession`] and a per-session mutex that
/// serializes concurrent agent turns for the same conversation.
pub struct SessionRegistry {
    sessions: DashMap<String, SessionEntry>,
    sessions_dir: PathBuf,
    working_dir: PathBuf,
    system_prompt: Option<String>,
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
    /// in `sessions_dir`.
    pub fn new(sessions_dir: PathBuf, working_dir: PathBuf, system_prompt: Option<String>) -> Self {
        Self {
            sessions: DashMap::new(),
            sessions_dir,
            working_dir,
            system_prompt,
        }
    }

    /// Build a deterministic session key from a gateway event.
    ///
    /// Format: `{platform}:{chat_type}:{chat_id}[:{thread_id}]`
    pub fn build_key(event: &GatewayEvent) -> String {
        let mut key = format!("{}:{}:{}", event.platform, event.chat_type, event.chat_id);
        if let Some(thread_id) = &event.thread_id {
            key.push(':');
            key.push_str(thread_id);
        }
        key
    }

    /// Get an existing session or create a new one.
    pub async fn get_or_create(&self, key: &str) -> SessionEntry {
        if let Some(entry) = self.sessions.get(key) {
            *entry.last_active.lock().unwrap() = Utc::now();
            return SessionEntry {
                session: entry.session.clone(),
                turn_lock: Mutex::new(()),
                created_at: entry.created_at,
                last_active: std::sync::Mutex::new(Utc::now()),
            };
        }

        let session_id = format_session_id(key);
        let store_path = self.sessions_dir.join(format!("{session_id}.json"));
        let system_message = self
            .system_prompt
            .as_deref()
            .map(perry_hermes_core::Message::system);

        let session = AgentSession::new(&session_id, &self.working_dir, system_message)
            .with_json_file_store(&store_path);

        let now = Utc::now();
        let entry = SessionEntry {
            session: session.clone(),
            turn_lock: Mutex::new(()),
            created_at: now,
            last_active: std::sync::Mutex::new(now),
        };

        self.sessions.insert(key.to_string(), entry);

        // Return a new entry handle (the DashMap owns the canonical one)
        SessionEntry {
            session,
            turn_lock: Mutex::new(()),
            created_at: now,
            last_active: std::sync::Mutex::new(now),
        }
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
    pub async fn get_session(&self, key: &str) -> Option<AgentSession> {
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

/// Format a session ID from a session key.
/// Replaces characters that are problematic in filenames.
fn format_session_id(key: &str) -> String {
    key.replace([':', '-'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ChatType, GatewayEvent};
    use chrono::Utc;

    fn make_event(platform: &str, chat_id: &str, chat_type: ChatType) -> GatewayEvent {
        GatewayEvent {
            platform: platform.into(),
            chat_id: chat_id.into(),
            chat_type,
            user_id: "user1".into(),
            user_name: None,
            thread_id: None,
            text: "hello".into(),
            message_id: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn build_key_dm() {
        let event = make_event("telegram", "123456", ChatType::Dm);
        assert_eq!(SessionRegistry::build_key(&event), "telegram:dm:123456");
    }

    #[test]
    fn build_key_group() {
        let event = make_event("telegram", "-100123", ChatType::Group);
        assert_eq!(SessionRegistry::build_key(&event), "telegram:group:-100123");
    }

    #[test]
    fn build_key_with_thread() {
        let mut event = make_event("telegram", "-100123", ChatType::Group);
        event.thread_id = Some("42".into());
        assert_eq!(
            SessionRegistry::build_key(&event),
            "telegram:group:-100123:42"
        );
    }

    #[test]
    fn format_session_id_replaces_colons() {
        assert_eq!(format_session_id("telegram:dm:123"), "telegram_dm_123");
    }

    #[tokio::test]
    async fn get_or_create_returns_same_session() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(tmp.path().into(), tmp.path().into(), None);

        let entry1 = registry.get_or_create("telegram:dm:123").await;
        let entry2 = registry.get_or_create("telegram:dm:123").await;

        assert_eq!(entry1.session.session_id, entry2.session.session_id);
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
        let session = registry.get_session("telegram:dm:123").await.unwrap();
        assert!(session.messages().await.is_empty());
    }

    #[tokio::test]
    async fn reset_nonexistent_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::new(tmp.path().into(), tmp.path().into(), None);
        assert!(!registry.reset("nonexistent").await);
    }
}
