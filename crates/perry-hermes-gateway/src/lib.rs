//! Platform gateway for Perry Hermes.
//!
//! This crate provides the gateway layer that bridges messaging platforms
//! (Telegram, Discord, etc.) with the Perry Hermes agent runtime. It
//! centralizes session management, message routing, and platform adapter
//! dispatch.
//!
//! # Architecture
//!
//! - [`GatewayRunner`] — central orchestrator: owns the [`AIAgent`] and
//!   [`SessionRegistry`], dispatches incoming events.
//! - [`PlatformAdapter`] — trait for platform-specific adapters.
//! - [`SessionRegistry`] — concurrent session store keyed by
//!   platform/chat identifiers.
//! - [`GatewayEvent`] — normalized incoming message from any platform.
//!
//! # Usage
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use perry_hermes_gateway::{
//!     GatewayConfig, GatewayRunner,
//!     telegram::TelegramAdapter,
//! };
//!
//! # fn example(agent: Arc<perry_hermes_agent::AIAgent>) {
//! let config = GatewayConfig::default();
//! let runner = GatewayRunner::new(agent, config);
//! let telegram = Arc::new(TelegramAdapter::new("BOT_TOKEN"));
//! // runner.run(vec![telegram]).await;
//! # }
//! ```

pub mod adapter;
pub mod config;
pub mod event;
pub mod runner;
pub mod session_registry;
pub mod telegram;

pub use adapter::PlatformAdapter;
pub use config::GatewayConfig;
pub use event::{ChatType, GatewayEvent};
pub use runner::{GatewayError, GatewayResponse, GatewayRunner};
pub use session_registry::{SessionEntry, SessionRegistry};
