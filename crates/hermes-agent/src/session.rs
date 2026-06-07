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

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub context: SessionContext,
    pub state: Arc<SessionState>,
    messages: Arc<RwLock<Vec<Message>>>,
}

impl AgentSession {
    pub fn new(context: SessionContext) -> Self {
        Self {
            context,
            state: Arc::new(SessionState::default()),
            messages: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn current_shell() -> Self {
        Self::new(SessionContext::current_shell())
    }

    pub async fn messages(&self) -> Vec<Message> {
        self.messages.read().await.clone()
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

    pub async fn reset(&self) {
        self.clear_messages().await;
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
