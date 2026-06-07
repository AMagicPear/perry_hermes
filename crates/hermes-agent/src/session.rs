use std::path::PathBuf;
use std::sync::Arc;

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
}

impl AgentSession {
    pub fn new(context: SessionContext) -> Self {
        Self {
            context,
            state: Arc::new(SessionState::default()),
        }
    }

    pub fn current_shell() -> Self {
        Self::new(SessionContext::current_shell())
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

    pub async fn reset(&self) {
        *self.first_prompt_context_tokens.write().await = None;
    }
}
