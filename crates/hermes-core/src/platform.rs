//! Concrete platform identifiers shared across the workspace.
//!
//! Used by:
//! - `Command::ALL` to mark which platforms a command is available on
//! - `GatewayEvent.platform` and the gateway `build_key` to namespace session keys
//! - the CLI's `new_cli_session_key` for the same purpose
//! - the `PlatformAdapter::name` contract
//!
//! `as_str()` is the canonical on-disk form (it is part of session-key strings
//! and on-disk file names). Keep it stable across releases — renaming a variant
//! without an on-disk migration would orphan existing session files.

/// A concrete platform. Variant names are Rust-idiomatic; the on-disk
/// string lives in [`Platform::as_str`] and may differ for backward
/// compatibility (e.g. `Tui` → `"cli"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// In-process TUI / CLI invocation.
    Tui,
    /// Tencent QQ Bot (gateway).
    QqBot,
    /// Telegram Bot (gateway).
    Telegram,
}

impl Platform {
    /// Canonical on-disk string. Used as the `platform` segment of session
    /// keys and as `PlatformAdapter::name()`. **Stable across releases.**
    pub const fn as_str(self) -> &'static str {
        match self {
            // Historical name — kept so existing `cli_*` session files load.
            Self::Tui => "cli",
            Self::QqBot => "qqbot",
            Self::Telegram => "telegram",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_is_stable_for_session_files() {
        // Changing any of these would break on-disk session resolution.
        assert_eq!(Platform::Tui.as_str(), "cli");
        assert_eq!(Platform::QqBot.as_str(), "qqbot");
        assert_eq!(Platform::Telegram.as_str(), "telegram");
    }

    #[test]
    fn display_matches_as_str() {
        for p in [Platform::Tui, Platform::QqBot, Platform::Telegram] {
            assert_eq!(format!("{p}"), p.as_str());
        }
    }
}
