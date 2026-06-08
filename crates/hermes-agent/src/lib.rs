//! Runtime engine shared by CLI and future gateways.

mod compaction;
mod config;
mod loop_engine;
mod prompting;
mod provider_factory;
mod runtime_agent;
mod session;
mod session_registry;
mod tool_catalog;

pub use compaction::{CompactorConfig, SummaryCompactor};
pub use config::{AgentConfig, ModelConfig, PerryHermesConfig, ProviderConfig, ProviderKind};
pub use loop_engine::{
    AgentLoop, AgentRunError, ContextWindow, FailedTurn, LoopConfig, LoopEvent, LoopMetrics,
    RunResult,
};
pub use runtime_agent::AIAgent;
pub use session::AgentSession;
pub use session_registry::{SessionEntry, SessionRegistry, default_sessions_dir};
