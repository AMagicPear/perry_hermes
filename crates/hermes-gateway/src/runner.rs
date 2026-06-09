use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use perry_hermes_agent::{AIAgent, AgentRunError, LoopEvent, SessionEntry, SessionRegistry};
use perry_hermes_core::Message;
use perry_hermes_core::commands::Command;

use crate::adapter::PlatformAdapter;
use crate::config::GatewayConfig;
use crate::event::GatewayEvent;

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

/// Central orchestrator that bridges platform adapters with the agent runtime.
pub struct GatewayRunner {
    agent: Arc<AIAgent>,
    sessions: Arc<SessionRegistry>,
    config: GatewayConfig,
}

/// Errors from processing a gateway event.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("user {user_id} is not allowed on platform {platform}")]
    Unauthorized { platform: String, user_id: String },
    #[error("agent run failed: {0}")]
    AgentRun(#[from] AgentRunError),
}

/// Response from processing a gateway event.
#[derive(Debug)]
pub enum GatewayResponse {
    /// Text to send back to the user.
    Reply(String),
    /// The event was ignored (e.g. unauthorized user, empty text).
    Ignored,
}

impl GatewayRunner {
    pub fn new(agent: Arc<AIAgent>, config: GatewayConfig) -> Self {
        let system_message = config.system_prompt.as_deref().map(Message::system);
        let sessions = Arc::new(SessionRegistry::new(
            config.sessions_dir.clone(),
            config.working_dir.clone(),
            system_message,
        ));
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
        sessions: Arc<SessionRegistry>,
    ) -> Self {
        Self {
            agent,
            sessions,
            config,
        }
    }

    /// Access the underlying agent.
    pub fn agent(&self) -> &Arc<AIAgent> {
        &self.agent
    }

    /// Access the session registry.
    pub fn sessions(&self) -> &Arc<SessionRegistry> {
        &self.sessions
    }

    /// Access the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// Process an incoming event from any platform adapter.
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

        // Command handling via the unified Command enum
        if let Some(cmd) = Command::parse(text) {
            return match cmd {
                Command::Reset | Command::New => {
                    let key = build_key(&event);
                    self.sessions.reset(&key).await;
                    info!(session = %key, "session reset by user");
                    Ok(GatewayResponse::Reply("Session has been reset.".into()))
                }
                Command::Compact(_) => self.handle_compact(&event).await,
                Command::Status => self.handle_status(&event).await,
                // CLI-only commands are ignored in the gateway
                Command::Quit | Command::Clear => Ok(GatewayResponse::Ignored),
            };
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

        // Build a GatewayRunner Arc that shares the same sessions
        let runner = Arc::new(GatewayRunner {
            agent: Arc::clone(&self.agent),
            sessions: Arc::clone(&self.sessions),
            config: self.config.clone(),
        });

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

    /// Lock and fetch the session entry for an event.
    async fn session(&self, event: &GatewayEvent) -> Arc<SessionEntry> {
        let key = build_key(event);
        self.sessions.get_or_create(&key).await
    }

    /// Process a normal user message through the agent loop.
    async fn handle_message(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let entry = self.session(event).await;
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
                Ok(GatewayResponse::Reply(response_text))
            }
            Err(e) => {
                let key = build_key(event);
                warn!(session = %key, error = %e, "agent run failed");
                Err(GatewayError::AgentRun(e))
            }
        }
    }

    /// Handle /compact command.
    async fn handle_compact(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let entry = self.session(event).await;
        let _guard = entry.turn_lock.lock().await;

        match self.agent.compact_session(&entry.session, None).await {
            Ok(event) => Ok(GatewayResponse::Reply(format!(
                "Compaction result: {event:?}"
            ))),
            Err(e) => {
                let key = build_key(event);
                warn!(session = %key, error = %e, "compaction failed");
                Ok(GatewayResponse::Reply(format!("Compaction failed: {e}")))
            }
        }
    }

    /// Handle /status command.
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
}

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
