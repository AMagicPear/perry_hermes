use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use perry_hermes_agent::{AIAgent, AgentRunError, LoopEvent};

use crate::adapter::PlatformAdapter;
use crate::config::GatewayConfig;
use crate::event::GatewayEvent;
use crate::session_registry::SessionRegistry;

/// Central orchestrator that bridges platform adapters with the agent runtime.
///
/// `GatewayRunner` owns the [`AIAgent`] and [`SessionRegistry`], and provides
/// `handle_event()` for adapters to dispatch incoming messages. It follows the
/// same architecture as Python's `GatewayRunner` but scoped to the Rust
/// agent's API surface.
pub struct GatewayRunner {
    agent: Arc<AIAgent>,
    sessions: SessionRegistry,
    config: GatewayConfig,
}

/// Errors from processing a gateway event.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("user {user_id} is not allowed on platform {platform}")]
    Unauthorized { platform: String, user_id: String },
    #[error("agent run failed: {0}")]
    AgentRun(#[from] AgentRunError),
    #[error("session error: {0}")]
    Session(String),
}

/// Response from processing a gateway event.
#[derive(Debug)]
pub enum GatewayResponse {
    /// A text response to send back to the user.
    Text(String),
    /// A command was handled; the adapter should send the inner text.
    CommandHandled(String),
    /// The event was ignored (e.g. unauthorized user, empty text).
    Ignored,
}

impl GatewayRunner {
    pub fn new(agent: Arc<AIAgent>, config: GatewayConfig) -> Self {
        let sessions = SessionRegistry::new(
            config.sessions_dir.clone(),
            config.working_dir.clone(),
            config.system_prompt.clone(),
        );
        Self {
            agent,
            sessions,
            config,
        }
    }

    /// Construct with an existing [`SessionRegistry`] (useful for testing).
    pub fn with_registry(
        agent: Arc<AIAgent>,
        config: GatewayConfig,
        sessions: SessionRegistry,
    ) -> Self {
        Self {
            agent,
            sessions,
            config,
        }
    }

    /// Access the underlying agent (for manual compaction, etc.).
    pub fn agent(&self) -> &Arc<AIAgent> {
        &self.agent
    }

    /// Access the session registry.
    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }

    /// Access the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// Process an incoming event from any platform adapter.
    ///
    /// Returns a [`GatewayResponse`] indicating what the adapter should do.
    /// The adapter is responsible for sending the response back to the user.
    pub async fn handle_event(&self, event: GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        // Authorization check
        if !self.config.is_user_allowed(&event.platform, &event.user_id) {
            return Err(GatewayError::Unauthorized {
                platform: event.platform.clone(),
                user_id: event.user_id.clone(),
            });
        }

        // Skip empty messages
        let text = event.text.trim();
        if text.is_empty() {
            return Ok(GatewayResponse::Ignored);
        }

        // Reset trigger check
        if self.config.is_reset_trigger(text) {
            let key = SessionRegistry::build_key(&event);
            self.sessions.reset(&key).await;
            info!(session = %key, "session reset by user");
            return Ok(GatewayResponse::CommandHandled(
                "Session has been reset.".into(),
            ));
        }

        // Command handling
        if text == "/compact" {
            return self.handle_compact(&event).await;
        }
        if text == "/status" {
            return self.handle_status(&event).await;
        }

        // Normal message: run through agent
        self.handle_message(&event).await
    }

    /// Run the gateway: connect all adapters and process messages until shutdown.
    pub async fn run(&self, adapters: Vec<Arc<dyn PlatformAdapter>>) -> anyhow::Result<()> {
        if adapters.is_empty() {
            anyhow::bail!("no platform adapters configured");
        }

        info!(count = adapters.len(), "starting platform adapters");

        let cancel = CancellationToken::new();

        // Build a GatewayRunner Arc to share with adapters
        let runner = Arc::new(GatewayRunner::with_registry(
            Arc::clone(&self.agent),
            self.config.clone(),
            SessionRegistry::new(
                self.config.sessions_dir.clone(),
                self.config.working_dir.clone(),
                self.config.system_prompt.clone(),
            ),
        ));

        // Run all adapters concurrently
        let mut handles = Vec::new();
        for adapter in &adapters {
            let adapter = Arc::clone(adapter);
            let runner = Arc::clone(&runner);
            handles.push(tokio::spawn(async move {
                if let Err(e) = adapter.run(runner).await {
                    warn!(adapter = adapter.name(), error = %e, "adapter exited with error");
                }
            }));
        }

        // Wait for shutdown signal (Ctrl+C)
        let shutdown_cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("shutdown signal received");
            shutdown_cancel.cancel();
        });

        // Wait for all adapters or cancellation
        tokio::select! {
            _ = async { for h in &mut handles { let _ = h.await; } } => {
                info!("all adapters exited");
            }
            _ = cancel.cancelled() => {
                info!("shutting down adapters");
                for adapter in &adapters {
                    if let Err(e) = adapter.disconnect().await {
                        warn!(adapter = adapter.name(), error = %e, "error disconnecting");
                    }
                }
            }
        }

        Ok(())
    }

    /// Process a normal user message through the agent loop.
    async fn handle_message(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let key = SessionRegistry::build_key(event);
        let entry = self.sessions.get_or_create(&key).await;

        // Serialize concurrent turns for this session
        let _guard = entry.turn_lock.lock().await;

        let cancel = CancellationToken::new();
        let mut response_text = String::new();

        let result = self
            .agent
            .run_session_turn(&event.text, &entry.session, cancel, |event| {
                if let LoopEvent::ContentDelta(delta) = event {
                    response_text.push_str(&delta);
                }
            })
            .await;

        match result {
            Ok(_) => {
                if response_text.is_empty() {
                    response_text = "(no response)".into();
                }
                Ok(GatewayResponse::Text(response_text))
            }
            Err(e) => {
                warn!(session = %key, error = %e, "agent run failed");
                Err(GatewayError::AgentRun(e))
            }
        }
    }

    /// Handle /compact command.
    async fn handle_compact(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let key = SessionRegistry::build_key(event);
        let entry = self.sessions.get_or_create(&key).await;
        let _guard = entry.turn_lock.lock().await;

        let result = self.agent.compact_session(&entry.session, None).await;

        match result {
            Ok(event) => {
                let msg = format!("Compaction result: {event:?}");
                Ok(GatewayResponse::CommandHandled(msg))
            }
            Err(e) => {
                warn!(session = %key, error = %e, "compaction failed");
                Ok(GatewayResponse::CommandHandled(format!(
                    "Compaction failed: {e}"
                )))
            }
        }
    }

    /// Handle /status command.
    async fn handle_status(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let key = SessionRegistry::build_key(event);
        let entry = self.sessions.get_or_create(&key).await;
        let messages = entry.session.messages().await;
        let msg_count = messages.len();

        let status = format!(
            "Session: {}\nMessages: {}\nWorking dir: {}",
            key,
            msg_count,
            entry.session.working_dir.display(),
        );
        Ok(GatewayResponse::CommandHandled(status))
    }
}

// Manual Clone because GatewayRunner contains non-Clone types
// but we wrap it in Arc so Clone is not needed.
