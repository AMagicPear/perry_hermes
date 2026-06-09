//! QQ Bot platform adapter.

pub mod adapter;
pub mod events;

pub use adapter::QQBotAdapter;
// QqBotConfig lives in `perry_hermes_agent` (it is the TOML schema for
// `[gateway.qqbot]`); re-exported for downstream convenience.
pub use perry_hermes_agent::QqBotConfig;
