//! QQ Bot platform adapter.

pub mod adapter;
pub mod config;
pub mod events;

pub use adapter::QQBotAdapter;
pub use config::{QqBotConfig, QqBotConfigError};
