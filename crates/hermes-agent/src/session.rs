use std::path::PathBuf;
use std::sync::Arc;

use perry_hermes_core::message::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionSnapshot {
    session_id: String,
    working_dir: PathBuf,
    system_message: Option<Message>,
    messages: Vec<Message>,
    /// First provider-reported prompt context usage for this session.
    /// This is used after compaction to estimate:
    /// `baseline + summary_output_tokens`.
    context_usage_baseline_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
struct JsonFileSessionStore {
    path: Arc<PathBuf>,
}

impl JsonFileSessionStore {
    fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
        }
    }

    async fn save(&self, snapshot: &SessionSnapshot) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(snapshot).map_err(std::io::Error::other)?;
        tokio::fs::write(self.path.as_ref(), bytes).await
    }
}

/// A single conversation.
///
/// `system_message` is the immutable system prompt for this session,
/// set at construction. It lives in its own field rather than at the
/// head of `messages` for two reasons:
///
/// 1. Compaction operates on `messages` only. Anchoring the system
///    prompt in a separate field means the compactor cannot
///    accidentally drop, reorder, or duplicate it. The post-compact
///    `messages` is always `[first_user, summary]`, and the
///    `outbound_messages` view is always `[system?, first_user,
///    summary]` regardless of how many compactions have run.
///
/// 2. Replacing the message log — e.g. after compaction or
///    session reset — never has to special-case the system entry.
///    `system_message` is set once and then read-only.
///
/// `messages` is the append-only business log: user, assistant, and
/// tool messages. It does not contain the system message.
#[derive(Debug, Clone)]
pub struct AgentSession {
    pub session_id: Arc<str>,
    pub working_dir: Arc<PathBuf>,
    pub system_message: Option<Arc<Message>>,
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

impl AgentSession {
    /// Create a new session. `system_message`, if provided, becomes
    /// the immutable system prompt for the lifetime of the session;
    /// it is stored in its own field and never appears in `messages`.
    pub fn new(
        session_id: impl Into<String>,
        working_dir: impl Into<PathBuf>,
        system_message: Option<Message>,
    ) -> Self {
        Self {
            session_id: Arc::from(session_id.into()),
            working_dir: Arc::new(working_dir.into()),
            system_message: system_message.map(Arc::new),
            messages: Arc::new(RwLock::new(Vec::with_capacity(8))),
            context_usage_baseline_tokens: Arc::new(RwLock::new(None)),
            store: None,
        }
    }

    pub fn with_json_file_store(self, path: impl Into<PathBuf>) -> Self {
        Self {
            store: Some(JsonFileSessionStore::new(path.into())),
            ..self
        }
    }

    /// Load saved session history and token facts from `path`.
    ///
    /// The runtime working directory is intentionally taken from the current
    /// process cwd at load time, so resuming a session follows the directory
    /// where the adapter was started. The saved `working_dir` remains part of
    /// the snapshot format but is not reused as the active tool context.
    pub async fn load_json_file(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::load_json_file_with_system_message(path, std::env::current_dir().ok(), None).await
    }

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
            messages: Arc::new(RwLock::new(snapshot.messages)),
            context_usage_baseline_tokens: Arc::new(RwLock::new(
                snapshot.context_usage_baseline_tokens,
            )),
            store: Some(JsonFileSessionStore::new(path)),
        })
    }

    pub async fn save(&self) -> std::io::Result<()> {
        self.save_snapshot().await
    }

    /// The business message log. Excludes the system message; it
    /// lives in `system_message` and is only reattached in
    /// `outbound_messages`.
    pub async fn messages(&self) -> Vec<Message> {
        self.messages.read().await.clone()
    }

    /// Full outbound view: `[system?, user, assistant, tool, ...]`,
    /// suitable for handing to a provider. Cloned so the caller can
    /// move it across an await without holding the session lock.
    pub async fn outbound_messages(&self) -> Vec<Message> {
        let log = self.messages.read().await;
        match &self.system_message {
            None => log.clone(),
            Some(sys) => {
                let mut out = Vec::with_capacity(log.len() + 1);
                out.push(Message::clone(sys));
                out.extend(log.iter().cloned());
                out
            }
        }
    }

    pub async fn append_message(&self, message: Message) {
        self.messages.write().await.push(message);
        self.persist().await;
    }

    pub async fn replace_messages(&self, messages: Vec<Message>) {
        *self.messages.write().await = messages;
        self.persist().await;
    }

    pub async fn clear_messages(&self) {
        self.messages.write().await.clear();
        self.persist().await;
    }

    /// Reset the business log and token-tracking state. The system
    /// message is unaffected (it lives in its own field) and will
    /// reappear at the head of `outbound_messages` on the next turn.
    pub async fn reset(&self) {
        self.messages.write().await.clear();
        self.reset_token_facts().await;
        self.persist().await;
    }

    /// Remember the first non-zero provider-reported prompt context usage.
    /// The loop records this once so compaction can estimate the immediate
    /// post-compact context usage as `baseline + summary_output_tokens`.
    pub async fn remember_context_usage_baseline(&self, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let mut guard = self.context_usage_baseline_tokens.write().await;
        if guard.is_none() {
            *guard = Some(tokens);
        }
        drop(guard);
        self.persist().await;
    }

    pub(crate) async fn compacted_context_tokens(&self, summary_output_tokens: u64) -> Option<u64> {
        self.context_usage_baseline_tokens
            .read()
            .await
            .map(|baseline| baseline.saturating_add(summary_output_tokens))
    }

    pub(crate) async fn reset_token_facts(&self) {
        *self.context_usage_baseline_tokens.write().await = None;
    }

    async fn persist(&self) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(err) = self.save_snapshot().await {
            tracing::warn!(
                "failed to persist session {} to {}: {err}",
                self.session_id,
                store.path.display()
            );
        }
    }

    async fn save_snapshot(&self) -> std::io::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store.save(&self.snapshot().await).await
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            session_id: self.session_id.to_string(),
            working_dir: self.working_dir.as_ref().clone(),
            system_message: self.system_message.as_deref().cloned(),
            messages: self.messages().await,
            context_usage_baseline_tokens: *self.context_usage_baseline_tokens.read().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_message_lives_in_its_own_field_not_in_messages() {
        let session = AgentSession::new(
            "s",
            PathBuf::from("/tmp"),
            Some(Message::system("system prompt")),
        );
        session.append_message(Message::user("first request")).await;

        // messages() does not include the system message.
        let log = session.messages().await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].content.as_text(), "first request");

        // outbound_messages() prepends the system message.
        let outbound = session.outbound_messages().await;
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[0].content.as_text(), "system prompt");
        assert_eq!(outbound[1].content.as_text(), "first request");
    }

    #[tokio::test]
    async fn reset_preserves_system_message_and_clears_business_log() {
        let session = AgentSession::new(
            "s",
            PathBuf::from("/tmp"),
            Some(Message::system("system prompt")),
        );
        session.append_message(Message::user("first")).await;
        session.append_message(Message::user("second")).await;
        session.reset().await;

        assert!(session.messages().await.is_empty());

        let outbound = session.outbound_messages().await;
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].content.as_text(), "system prompt");
    }

    #[tokio::test]
    async fn replace_messages_does_not_touch_system_message() {
        let session = AgentSession::new(
            "s",
            PathBuf::from("/tmp"),
            Some(Message::system("system prompt")),
        );

        // Simulate post-compaction state: business log becomes
        // [first_user, summary].
        session
            .replace_messages(vec![
                Message::user("first"),
                Message::user("[SUMMARY] condensed"),
            ])
            .await;

        let outbound = session.outbound_messages().await;
        assert_eq!(outbound.len(), 3);
        assert_eq!(outbound[0].content.as_text(), "system prompt");
        assert_eq!(outbound[1].content.as_text(), "first");
        assert_eq!(outbound[2].content.as_text(), "[SUMMARY] condensed");
    }

    #[tokio::test]
    async fn session_owns_identity_and_working_directory_directly() {
        let session = AgentSession::new("s", PathBuf::from("/tmp/project"), None);

        assert_eq!(session.session_id.as_ref(), "s");
        assert_eq!(session.working_dir.as_ref(), &PathBuf::from("/tmp/project"));
    }

    #[tokio::test]
    async fn json_file_store_persists_provider_neutral_session_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sessions").join("session-1.json");
        let session = AgentSession::new(
            "session-1",
            PathBuf::from("/tmp/project"),
            Some(Message::system("system prompt")),
        )
        .with_json_file_store(path.clone());

        session.append_message(Message::user("hello")).await;
        session.remember_context_usage_baseline(123).await;

        let raw = tokio::fs::read_to_string(&path)
            .await
            .expect("session snapshot should be written");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["working_dir"], "/tmp/project");
        assert_eq!(value["system_message"]["role"], "system");
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"], "hello");
        assert_eq!(value["context_usage_baseline_tokens"], 123);
        assert!(value.get("provider").is_none());
        assert!(value.get("model").is_none());
    }

    #[tokio::test]
    async fn json_file_store_can_persist_new_empty_session() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sessions").join("session-1.json");
        let session = AgentSession::new("session-1", PathBuf::from("/tmp/project"), None)
            .with_json_file_store(path.clone());

        session.save().await.unwrap();

        let raw = tokio::fs::read_to_string(&path)
            .await
            .expect("empty session snapshot should be written");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(
            value["messages"].as_array().map(Vec::len),
            Some(0),
            "new session should save an empty provider-neutral history"
        );
    }

    #[tokio::test]
    async fn json_file_store_loads_history_but_uses_current_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sessions").join("session-1.json");
        let original = AgentSession::new(
            "session-1",
            PathBuf::from("/tmp/original-project"),
            Some(Message::system("persisted system prompt")),
        )
        .with_json_file_store(path.clone());
        original.append_message(Message::user("hello")).await;
        original.remember_context_usage_baseline(123).await;

        let current_cwd = std::env::current_dir().unwrap();
        let restored = AgentSession::load_json_file(path)
            .await
            .expect("snapshot should load");

        assert_eq!(restored.session_id.as_ref(), "session-1");
        assert_eq!(restored.working_dir.as_ref(), &current_cwd);

        let outbound = restored.outbound_messages().await;
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[0].content.as_text(), "persisted system prompt");
        assert_eq!(outbound[1].content.as_text(), "hello");
        assert_eq!(restored.compacted_context_tokens(7).await, Some(130));
    }
}
