//! Runtime wiring shared by CLI and future gateways.

mod agent;
pub mod config;
mod prompting;
mod provider_factory;
mod tool_catalog;

pub use agent::{AIAgent, SessionContext};
pub use config::{AgentConfig, HermesConfig, ProviderConfig, ProviderKind};
pub use hermes_loop::LoopEvent;
pub use prompting::DEFAULT_SYSTEM_PROMPT;
