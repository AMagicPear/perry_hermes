//! Runtime engine shared by CLI and future gateways.

mod config;
mod loop_engine;
mod prompting;
mod provider_factory;
mod runtime_agent;
mod session;
mod tool_catalog;
pub mod tools;

pub use config::{AgentConfig, HermesConfig, ModelConfig, ProviderConfig, ProviderKind};
pub use loop_engine::{
    AgentLoop, AgentRunError, CompactorConfig, ContextWindow, FailedTurn, LoopConfig, LoopEvent,
    LoopMetrics, RunResult, SummaryCompactor,
};
pub use runtime_agent::AIAgent;
pub use session::{AgentSession, SessionContext, SessionState};
