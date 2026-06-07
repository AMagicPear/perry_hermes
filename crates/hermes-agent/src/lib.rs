//! Runtime engine shared by CLI and future gateways.

mod config;
pub mod context;
mod loop_engine;
mod prompting;
mod provider_factory;
mod runtime_agent;
mod session;
mod tool_catalog;
pub mod tools;

pub use config::{AgentConfig, HermesConfig, ModelConfig, ProviderConfig, ProviderKind};
pub use context::{CompressorConfig, ContextCompressor};
pub use loop_engine::{
    estimate_tokens_for_messages, AgentLoop, AgentRunError, FailedTurn, LoopConfig, LoopEvent,
    LoopMetrics, RunResult,
};
pub use runtime_agent::AIAgent;
pub use session::SessionContext;
