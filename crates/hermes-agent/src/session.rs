use std::path::PathBuf;
use std::sync::Arc;

use perry_hermes_core::message::Message;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct SessionContext {
    pub working_dir: PathBuf,
    pub session_id: String,
}

impl SessionContext {
    pub fn current_shell() -> Self {
        Self {
            working_dir: std::env::current_dir().unwrap_or_default(),
            session_id: "shell".into(),
        }
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
    pub context: SessionContext,
    pub state: Arc<SessionState>,
    system_message: Option<Arc<Message>>,
    messages: Arc<RwLock<Vec<Message>>>,
}

impl AgentSession {
    /// Create a new session. `system_message`, if provided, becomes
    /// the immutable system prompt for the lifetime of the session;
    /// it is stored in its own field and never appears in `messages`.
    pub fn new(context: SessionContext, system_message: Option<Message>) -> Self {
        Self {
            context,
            state: Arc::new(SessionState::default()),
            system_message: system_message.map(Arc::new),
            messages: Arc::new(RwLock::new(Vec::with_capacity(8))),
        }
    }

    pub fn current_shell() -> Self {
        Self::new(SessionContext::current_shell(), None)
    }

    /// The immutable system message for this session, if any.
    pub fn system_message(&self) -> Option<&Message> {
        self.system_message.as_deref()
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
    }

    pub async fn replace_messages(&self, messages: Vec<Message>) {
        *self.messages.write().await = messages;
    }

    pub async fn clear_messages(&self) {
        self.messages.write().await.clear();
    }

    /// Reset the business log and token-tracking state. The system
    /// message is unaffected (it lives in its own field) and will
    /// reappear at the head of `outbound_messages` on the next turn.
    pub async fn reset(&self) {
        self.messages.write().await.clear();
        self.state.reset().await;
    }
}

#[derive(Debug, Default)]
pub struct SessionState {
    first_prompt_context_tokens: RwLock<Option<u64>>,
}

impl SessionState {
    pub async fn remember_first_prompt_context_tokens(&self, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let mut guard = self.first_prompt_context_tokens.write().await;
        if guard.is_none() {
            *guard = Some(tokens);
        }
    }

    pub async fn first_prompt_context_tokens(&self) -> Option<u64> {
        *self.first_prompt_context_tokens.read().await
    }

    pub async fn compacted_context_tokens(&self, summary_output_tokens: u64) -> Option<u64> {
        self.first_prompt_context_tokens()
            .await
            .map(|baseline| baseline.saturating_add(summary_output_tokens))
    }

    pub async fn reset(&self) {
        *self.first_prompt_context_tokens.write().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    fn system_msg(text: &str) -> Message {
        Message::system(text)
    }

    #[tokio::test]
    async fn system_message_lives_in_its_own_field_not_in_messages() {
        let session = AgentSession::new(
            SessionContext {
                working_dir: PathBuf::from("/tmp"),
                session_id: "s".into(),
            },
            Some(system_msg("system prompt")),
        );
        session.append_message(user_msg("first request")).await;

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
            SessionContext {
                working_dir: PathBuf::from("/tmp"),
                session_id: "s".into(),
            },
            Some(system_msg("system prompt")),
        );
        session.append_message(user_msg("first")).await;
        session.append_message(user_msg("second")).await;
        session.reset().await;

        assert!(session.messages().await.is_empty());

        let outbound = session.outbound_messages().await;
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].content.as_text(), "system prompt");
    }

    #[tokio::test]
    async fn replace_messages_does_not_touch_system_message() {
        let session = AgentSession::new(
            SessionContext {
                working_dir: PathBuf::from("/tmp"),
                session_id: "s".into(),
            },
            Some(system_msg("system prompt")),
        );

        // Simulate post-compaction state: business log becomes
        // [first_user, summary].
        session
            .replace_messages(vec![user_msg("first"), user_msg("[SUMMARY] condensed")])
            .await;

        let outbound = session.outbound_messages().await;
        assert_eq!(outbound.len(), 3);
        assert_eq!(outbound[0].content.as_text(), "system prompt");
        assert_eq!(outbound[1].content.as_text(), "first");
        assert_eq!(outbound[2].content.as_text(), "[SUMMARY] condensed");
    }
}
