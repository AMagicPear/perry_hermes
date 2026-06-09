use crate::platform::Platform;

/// Project-wide slash command enum.
///
/// Covers all commands across all platforms (TUI, QQ Bot, Telegram).
/// Each platform matches the variants it cares about and ignores the rest.
/// `Command::parse` is the single parser for `/command [args]` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/reset` — reset the current session.
    Reset,
    /// `/new` — alias for `/reset`.
    New,
    /// `/compact [focus]` — compact conversation context.
    Compact(Option<String>),
    /// `/status` — show session status.
    Status,
    /// `/quit` or `/exit` — exit the application (TUI only).
    Quit,
    /// `/clear` — clear scrollback (TUI only).
    Clear,
}

impl Command {
    /// Parse a `/command [args]` string into a `Command`. Returns `None`
    /// for unrecognized commands.
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match cmd {
            "/reset" => Some(Self::Reset),
            "/new" => Some(Self::New),
            "/compact" => Some(Self::Compact(if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            })),
            "/status" => Some(Self::Status),
            "/quit" | "/exit" => Some(Self::Quit),
            "/clear" => Some(Self::Clear),
            _ => None,
        }
    }

    /// Canonical metadata for every command — single source of truth for
    /// help text and platform registration (e.g. the Telegram `/` menu).
    /// Descriptions live with the data they describe, not on individual
    /// variants, since they don't depend on `Compact`'s arg.
    pub const ALL: &'static [CommandMeta] = &[
        CommandMeta {
            name: "reset",
            description: "Reset the current session",
            platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
        },
        CommandMeta {
            name: "new",
            description: "Reset the current session (alias)",
            platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
        },
        CommandMeta {
            name: "compact",
            description: "Compact the conversation context",
            platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
        },
        CommandMeta {
            name: "status",
            description: "Show session status",
            platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
        },
        CommandMeta {
            name: "quit",
            description: "Exit the application",
            platforms: &[Platform::Tui],
        },
        CommandMeta {
            name: "clear",
            description: "Clear scrollback",
            platforms: &[Platform::Tui],
        },
    ];

    /// Metadata for the commands available on `platform`, in `ALL` order.
    /// Single entry point adapters (and help text generators) use to discover
    /// their supported command set — no adapter needs to know which specific
    /// names belong to which platform.
    pub fn for_platform(platform: Platform) -> impl Iterator<Item = &'static CommandMeta> {
        Self::ALL.iter().filter(move |m| m.platforms.contains(&platform))
    }
}

/// Static description of a single command. The shape of the constant slice
/// `Command::ALL`, so all fields are `&'static str` / `&'static [Platform]`.
#[derive(Debug, Clone, Copy)]
pub struct CommandMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub platforms: &'static [Platform],
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compact(Some(focus)) => write!(f, "/compact {focus}"),
            Self::Compact(None) => f.write_str("/compact"),
            Self::Reset => f.write_str("/reset"),
            Self::New => f.write_str("/new"),
            Self::Status => f.write_str("/status"),
            Self::Quit => f.write_str("/quit"),
            Self::Clear => f.write_str("/clear"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basics() {
        assert_eq!(Command::parse("/reset"), Some(Command::Reset));
        assert_eq!(Command::parse("/new"), Some(Command::New));
        assert_eq!(Command::parse("/status"), Some(Command::Status));
        assert_eq!(Command::parse("/quit"), Some(Command::Quit));
        assert_eq!(Command::parse("/exit"), Some(Command::Quit));
        assert_eq!(Command::parse("/clear"), Some(Command::Clear));
    }

    #[test]
    fn parse_compact() {
        assert_eq!(Command::parse("/compact"), Some(Command::Compact(None)));
        assert_eq!(
            Command::parse("/compact focus on shell"),
            Some(Command::Compact(Some("focus on shell".into())))
        );
    }

    #[test]
    fn parse_unknown() {
        assert_eq!(Command::parse("/bogus"), None);
        assert_eq!(Command::parse("hello"), None);
    }

    #[test]
    fn display_round_trips_for_non_arg_commands() {
        for cmd in [
            Command::Reset,
            Command::New,
            Command::Status,
            Command::Quit,
            Command::Clear,
        ] {
            assert_eq!(Command::parse(&format!("{cmd}")), Some(cmd));
        }
    }

    #[test]
    fn all_lists_every_variant_with_unique_names() {
        use std::collections::HashSet;

        // One entry per variant.
        assert_eq!(Command::ALL.len(), 6);

        // Names are unique — protects `for_platform`-style filters that key on name.
        let names: HashSet<&str> = Command::ALL.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), Command::ALL.len(), "duplicate name in ALL");

        // Every entry's name actually parses — keeps ALL and `parse` in sync.
        for m in Command::ALL {
            assert!(
                Command::parse(&format!("/{}", m.name)).is_some(),
                "ALL entry '{}' is not a parseable command",
                m.name,
            );
        }
    }

    #[test]
    fn for_platform_splits_tui_from_gateways() {
        // TUI sees everything.
        let tui: Vec<&str> = Command::for_platform(Platform::Tui).map(|m| m.name).collect();
        assert_eq!(tui.len(), Command::ALL.len());
        assert!(tui.contains(&"quit"));
        assert!(tui.contains(&"clear"));

        // Each gateway excludes the TUI-only `/quit` and `/clear`.
        for gw in [Platform::QqBot, Platform::Telegram] {
            let names: Vec<&str> = Command::for_platform(gw).map(|m| m.name).collect();
            assert_eq!(
                names.len(),
                Command::ALL.len() - 2,
                "gateway {gw:?} should drop exactly the two TUI-only commands",
            );
            assert!(!names.contains(&"quit"), "{gw:?} leaked /quit");
            assert!(!names.contains(&"clear"), "{gw:?} leaked /clear");
        }

        // The four shared gateway commands appear in ALL order.
        let tg: Vec<&str> = Command::for_platform(Platform::Telegram).map(|m| m.name).collect();
        assert_eq!(tg, vec!["reset", "new", "compact", "status"]);

        // Every command lists at least one platform.
        for m in Command::ALL {
            assert!(!m.platforms.is_empty(), "ALL entry '{}' has no platform", m.name);
        }
    }
}
