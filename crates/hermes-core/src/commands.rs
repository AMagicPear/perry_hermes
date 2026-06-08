/// Project-wide slash command enum.
///
/// Covers all commands across all platforms (CLI, Telegram, etc.).
/// Each platform matches the variants it cares about and ignores the rest.
/// `Command::parse` is the single parser for `/command [args]` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/reset` — reset the current session (gateway).
    Reset,
    /// `/new` — alias for `/reset` (gateway).
    New,
    /// `/compact [focus]` — compact conversation context (CLI + gateway).
    Compact(Option<String>),
    /// `/status` — show session status (gateway).
    Status,
    /// `/quit` or `/exit` — exit the application (CLI).
    Quit,
    /// `/clear` — clear scrollback (CLI).
    Clear,
}

impl Command {
    /// Parse a `/command [args]` string into a `Command`.
    /// Returns `None` for unrecognized commands.
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

    /// Human-readable description for help / registration.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Reset => "Reset the current session",
            Self::New => "Reset the current session (alias)",
            Self::Compact(_) => "Compact the conversation context",
            Self::Status => "Show session status",
            Self::Quit => "Exit the application",
            Self::Clear => "Clear scrollback",
        }
    }
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
}
