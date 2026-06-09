pub mod adapter;

pub use adapter::TelegramAdapter;
// TelegramConfig lives in `perry_hermes_agent` (it is the TOML schema for
// `[gateway.telegram]`); re-exported for downstream convenience.
pub use perry_hermes_agent::TelegramConfig;
