use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use perry_hermes_agent::{AgentLoop, AgentRunError, LoopEvent, SessionEntry, SessionRegistry};
use perry_hermes_core::Platform;
use perry_hermes_core::commands::Command;

use crate::adapter::PlatformAdapter;
use crate::config::GatewayConfig;
use crate::event::GatewayEvent;
use crate::handler::{GatewayEventHandler, dispatch_loop_event};

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
    agent: Arc<AgentLoop>,
    sessions: Arc<SessionRegistry>,
    config: GatewayConfig,
}

/// Errors from processing a gateway event.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("user {user_id} is not allowed on platform {platform}")]
    Unauthorized { platform: Platform, user_id: String },
    #[error("agent run failed: {0}")]
    AgentRun(#[from] AgentRunError),
}

/// Response from processing a gateway event.
#[derive(Debug)]
pub enum GatewayResponse {
    /// A one-shot text response (e.g. slash command). For agent messages,
    /// streaming content is delivered through [`GatewayEventHandler`].
    CommandReply(String),
    /// The event was ignored (e.g. unauthorized user, empty text).
    Ignored,
}

impl GatewayRunner {
    pub fn new(agent: Arc<AgentLoop>, config: GatewayConfig) -> Self {
        // One-shot startup call — block_on is acceptable here.
        let system_message =
            futures::executor::block_on(agent.system_message_for(&config.working_dir));
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

    /// Access the underlying agent.
    pub fn agent(&self) -> &Arc<AgentLoop> {
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
    ///
    /// For normal messages, agent output is streamed through `handler`.
    /// For slash commands, a one-shot `CommandReply` is returned.
    pub async fn handle_event(
        &self,
        event: GatewayEvent,
        handler: &mut dyn GatewayEventHandler,
    ) -> Result<GatewayResponse, GatewayError> {
        // Authorization check — config keys on the platform's on-disk string form.
        if !self
            .config
            .is_user_allowed(event.platform.as_str(), &event.user_id)
        {
            return Err(GatewayError::Unauthorized {
                platform: event.platform,
                user_id: event.user_id.clone(),
            });
        }

        // Skip empty messages
        let text = event.text.trim();
        if text.is_empty() {
            return Ok(GatewayResponse::Ignored);
        }

        // Command handling via the unified Command enum
        if let Some(parsed) = Command::parse(text) {
            return match parsed.command {
                Command::Reset | Command::New => {
                    let key = build_key(&event);
                    self.sessions.reset(&key).await;
                    info!(session = %key, "session reset by user");
                    Ok(GatewayResponse::CommandReply(
                        "Session has been reset.".into(),
                    ))
                }
                Command::Compact => self.handle_compact(&event, parsed.arg.as_deref()).await,
                Command::Status => self.handle_status(&event).await,
                // CLI-only commands are ignored in the gateway
                Command::Quit | Command::Clear => Ok(GatewayResponse::Ignored),
            };
        }

        // Normal message: run through agent, streaming to handler
        self.handle_message(&event, handler).await
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
    ///
    /// Agent events are dispatched to `handler` as they arrive.
    /// Content is delivered incrementally — the handler decides when
    /// to flush (typically at tool call boundaries and turn completion).
    async fn handle_message(
        &self,
        event: &GatewayEvent,
        handler: &mut dyn GatewayEventHandler,
    ) -> Result<GatewayResponse, GatewayError> {
        let entry = self.session(event).await;
        let _guard = entry.turn_lock.lock().await;

        let cancel = CancellationToken::new();
        let mut response_empty = true;

        let result = self
            .agent
            .run_session_turn(&event.text, &entry.session, cancel, |event| {
                if matches!(event, LoopEvent::ContentDelta(_)) {
                    response_empty = false;
                }
                dispatch_loop_event(handler, &event);
            })
            .await;

        match result {
            Ok(_) => {
                if response_empty {
                    handler.on_content_delta("(no response)");
                }
                handler.on_turn_completed();
                Ok(GatewayResponse::Ignored)
            }
            Err(e) => {
                let key = build_key(event);
                warn!(session = %key, error = %e, "agent run failed");
                handler.on_error(&format!("{e}"));
                handler.on_turn_completed();
                Err(GatewayError::AgentRun(e))
            }
        }
    }

    /// Handle /compact command. `focus` is the optional argument the user
    /// passed (e.g. the `shell` in `/compact shell`); passed through to
    /// the agent so it can bias the compaction summary.
    async fn handle_compact(
        &self,
        event: &GatewayEvent,
        focus: Option<&str>,
    ) -> Result<GatewayResponse, GatewayError> {
        let entry = self.session(event).await;
        let _guard = entry.turn_lock.lock().await;

        match self.agent.compact_session(&entry.session, focus).await {
            Ok(event) => Ok(GatewayResponse::CommandReply(format!(
                "Compaction result: {event:?}"
            ))),
            Err(e) => {
                let key = build_key(event);
                warn!(session = %key, error = %e, "compaction failed");
                Ok(GatewayResponse::CommandReply(format!(
                    "Compaction failed: {e}"
                )))
            }
        }
    }

    /// Handle /status command.
    async fn handle_status(&self, event: &GatewayEvent) -> Result<GatewayResponse, GatewayError> {
        let entry = self.session(event).await;
        let messages = entry.session.messages().await;
        let key = build_key(event);
        let archive_dir = self.config.sessions_dir.join(".archive");
        let archived = count_files_in(&archive_dir).await;

        Ok(GatewayResponse::CommandReply(format!(
            "Session: {}\nMessages: {}\nWorking dir: {}\nArchived: {}",
            key,
            messages.len(),
            entry.session.working_dir.display(),
            archived,
        )))
    }
}

/// Count the regular files in `dir`. Returns 0 if the directory
/// does not exist. Non-recoverable filesystem errors are logged
/// via `tracing::warn!` and the scan aborts at the point of
/// failure (returning the count seen so far).
async fn count_files_in(dir: &Path) -> u64 {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(error = %err, dir = %dir.display(), "cannot read archive dir");
            return 0;
        }
    };
    let mut n = 0u64;
    loop {
        match rd.next_entry().await {
            Ok(Some(entry)) => {
                let is_file = matches!(
                    entry.file_type().await,
                    Ok(ft) if ft.is_file()
                );
                if is_file {
                    n += 1;
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(error = %err, dir = %dir.display(), "archive dir scan aborted");
                break;
            }
        }
    }
    n
}

#[cfg(test)]
mod count_files_in_tests {
    use super::count_files_in;

    #[tokio::test]
    async fn empty_dir_returns_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(count_files_in(tmp.path()).await, 0);
    }

    #[tokio::test]
    async fn counts_regular_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), b"x").unwrap();
        std::fs::write(tmp.path().join("b.json"), b"x").unwrap();
        std::fs::create_dir(tmp.path().join("nested")).unwrap();
        // nested/ is a directory; should not inflate the count.
        assert_eq!(count_files_in(tmp.path()).await, 2);
    }

    #[tokio::test]
    async fn nonexistent_dir_returns_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        assert_eq!(count_files_in(&missing).await, 0);
    }
}
