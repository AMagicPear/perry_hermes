//! Platform gateway for Perry Hermes.
//!
//! This crate provides the gateway layer that bridges messaging platforms
//! (Telegram, QQ Bot, Discord, etc.) with the Perry Hermes agent runtime. It
//! centralizes session management, message routing, and platform adapter
//! dispatch.
//!
//! # Architecture
//!
//! - [`GatewayRunner`] — central orchestrator: owns the [`AIAgent`] and
//!   [`SessionRegistry`], dispatches incoming events.
//! - [`PlatformAdapter`] — trait for platform-specific adapters.
//! - [`SessionRegistry`] — concurrent session store keyed by
//!   platform/chat identifiers (re-exported from `perry-hermes-agent`).
//! - [`GatewayEvent`] — normalized incoming message from any platform.
//!
//! # Usage
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use perry_hermes_gateway::{
//!     GatewayConfig, GatewayRunner,
//!     telegram::TelegramAdapter,
//!     QQBotAdapter, QqBotConfig,
//! };
//!
//! # fn example(agent: Arc<perry_hermes_agent::AIAgent>) {
//! let config = GatewayConfig::default();
//! let runner = GatewayRunner::new(agent, config);
//! let telegram = Arc::new(TelegramAdapter::new("BOT_TOKEN"));
//! let qqbot = Arc::new(QQBotAdapter::new(QqBotConfig::default()));
//! // runner.run(vec![telegram, qqbot]).await;
//! # }
//! ```

pub mod adapter;
pub mod config;
pub mod event;
pub mod runner;
pub mod telegram;
pub mod qqbot;

pub use adapter::PlatformAdapter;
pub use config::GatewayConfig;
pub use event::{ChatType, GatewayEvent};
// Re-export the project-wide Command enum from hermes-core.
pub use perry_hermes_core::commands::Command;
pub use qqbot::{QQBotAdapter, QqBotConfig, QqBotConfigError};
pub use runner::{GatewayError, GatewayResponse, GatewayRunner};
// Re-export session types from hermes-agent for convenience.
pub use perry_hermes_agent::{SessionEntry, SessionRegistry};
