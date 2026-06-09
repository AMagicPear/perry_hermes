use std::mem;

use crate::platform::Platform;

/// Project-wide slash command identity.
///
/// All variants are unit — commands are *identities* (which one was
/// invoked), not carriers of their arguments. Any trailing argument
/// (e.g. `/compact <focus>`) lives in [`ParsedCommand::arg`], not on the
/// variant. This keeps the enum from growing a new variant every time
/// we add a command that takes a string.
///
/// Variants are paired with their metadata in [`Command::ALL`], which
/// is the single source of truth for names, descriptions, and platform
/// availability. [`Command::parse`] and the [`Display`] impl both go
/// through that dictionary rather than re-encoding name strings locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// `/reset` — reset the current session.
    Reset,
    /// `/new` — alias for `/reset`.
    New,
    /// `/compact [focus]` — compact conversation context.
    Compact,
    /// `/status` — show session status.
    Status,
    /// `/quit` or `/exit` — exit the application (TUI only).
    Quit,
    /// `/clear` — clear scrollback (TUI only).
    Clear,
}

/// The result of [`Command::parse`]: a command identity plus its optional
/// trailing argument. `arg` is `Some` only when the input had content
/// after the command name; most commands leave it `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub command: Command,
    pub arg: Option<String>,
}

impl Command {
    /// The dictionary: every variant paired with its metadata. Adding a
    /// command means adding a variant to the enum and one tuple entry
    /// here — `parse`, `Display`, `for_platform`, and `meta` all derive
    /// their behavior from this list.
    pub const ALL: &'static [(Self, CommandMeta)] = &[
        (
            Self::Reset,
            CommandMeta {
                name: "reset",
                description: "Reset the current session",
                platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
            },
        ),
        (
            Self::New,
            CommandMeta {
                name: "new",
                description: "Reset the current session (alias)",
                platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
            },
        ),
        (
            Self::Compact,
            CommandMeta {
                name: "compact",
                description: "Compact the conversation context",
                platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
            },
        ),
        (
            Self::Status,
            CommandMeta {
                name: "status",
                description: "Show session status",
                platforms: &[Platform::Tui, Platform::QqBot, Platform::Telegram],
            },
        ),
        (
            Self::Quit,
            CommandMeta {
                name: "quit",
                description: "Exit the application",
                platforms: &[Platform::Tui],
            },
        ),
        (
            Self::Clear,
            CommandMeta {
                name: "clear",
                description: "Clear scrollback",
                platforms: &[Platform::Tui],
            },
        ),
    ];

    /// Look up the metadata for this variant. O(n) over `ALL` (n=6) by
    /// discriminant — fine for the size.
    pub fn meta(&self) -> &'static CommandMeta {
        let d = mem::discriminant(self);
        Self::ALL
            .iter()
            .find(|(c, _)| mem::discriminant(c) == d)
            .map(|(_, m)| m)
            .expect("ALL must contain an entry for every Command variant")
    }

    /// Metadata for the commands available on `platform`, in `ALL` order.
    /// Single entry point adapters (and help text generators) use to
    /// discover their supported command set — no caller needs to know
    /// which specific names belong to which platform.
    pub fn for_platform(platform: Platform) -> impl Iterator<Item = &'static CommandMeta> {
        Self::ALL
            .iter()
            .filter(move |(_, m)| m.platforms.contains(&platform))
            .map(|(_, m)| m)
    }

    /// Parse a `/command [args]` string. Returns `None` for unrecognized
    /// commands. The known names live in [`Command::ALL`] — this function
    /// does not embed any name string literals of its own (except the
    /// single historical alias `/exit` → `/quit`).
    pub fn parse(input: &str) -> Option<ParsedCommand> {
        let trimmed = input.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("").trim_start_matches('/');
        let rest = parts.next().unwrap_or("").trim();

        // The only historical alias. Kept here rather than in `ALL` so
        // that the dictionary holds one canonical name per variant.
        let canonical = if cmd == "exit" { "quit" } else { cmd };

        Self::ALL.iter().find_map(|(command, m)| {
            if m.name != canonical {
                return None;
            }
            Some(ParsedCommand {
                command: *command,
                arg: if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                },
            })
        })
    }
}

/// Static description of a single command. The value type of the
/// `Command::ALL` dictionary, so all fields are `&'static`.
#[derive(Debug, Clone, Copy)]
pub struct CommandMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub platforms: &'static [Platform],
}

impl std::fmt::Display for ParsedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.command.meta();
        match &self.arg {
            Some(arg) => write!(f, "/{} {}", m.name, arg),
            None => write!(f, "/{}", m.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::mem::discriminant;

    fn parsed(command: Command, arg: Option<&str>) -> ParsedCommand {
        ParsedCommand {
            command,
            arg: arg.map(String::from),
        }
    }

    #[test]
    fn parse_basics() {
        assert_eq!(Command::parse("/reset"), Some(parsed(Command::Reset, None)));
        assert_eq!(Command::parse("/new"), Some(parsed(Command::New, None)));
        assert_eq!(Command::parse("/status"), Some(parsed(Command::Status, None)));
        assert_eq!(Command::parse("/quit"), Some(parsed(Command::Quit, None)));
        // Historical alias for /quit.
        assert_eq!(Command::parse("/exit"), Some(parsed(Command::Quit, None)));
        assert_eq!(Command::parse("/clear"), Some(parsed(Command::Clear, None)));
    }

    #[test]
    fn parse_compact_carries_arg_separately_from_variant() {
        assert_eq!(Command::parse("/compact"), Some(parsed(Command::Compact, None)));
        assert_eq!(
            Command::parse("/compact focus on shell"),
            Some(parsed(Command::Compact, Some("focus on shell"))),
        );
        // Same variant, different arg — the variant alone doesn't change.
        assert_eq!(
            Command::parse("/compact foo"),
            Some(parsed(Command::Compact, Some("foo"))),
        );
    }

    #[test]
    fn parse_unknown() {
        assert_eq!(Command::parse("/bogus"), None);
        assert_eq!(Command::parse("hello"), None);
    }

    #[test]
    fn display_round_trips_for_arg_less_commands() {
        for cmd in [
            Command::Reset,
            Command::New,
            Command::Status,
            Command::Quit,
            Command::Clear,
        ] {
            let p = parsed(cmd, None);
            assert_eq!(Command::parse(&format!("{p}")), Some(p));
        }
    }

    #[test]
    fn display_round_trips_for_compact_with_arg() {
        let p = parsed(Command::Compact, Some("focus on shell"));
        assert_eq!(format!("{p}"), "/compact focus on shell");
        assert_eq!(Command::parse(&format!("{p}")), Some(p));
    }

    #[test]
    fn all_covers_every_variant_exactly_once() {
        // One entry per Command variant — and every variant is present.
        let in_all: HashSet<_> = Command::ALL.iter().map(|(c, _)| discriminant(c)).collect();
        let expected: HashSet<_> = [
            discriminant(&Command::Reset),
            discriminant(&Command::New),
            discriminant(&Command::Compact),
            discriminant(&Command::Status),
            discriminant(&Command::Quit),
            discriminant(&Command::Clear),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            in_all, expected,
            "ALL must contain exactly one entry per Command variant"
        );

        // Names are unique — protects `parse`'s name lookup.
        let names: HashSet<&str> = Command::ALL.iter().map(|(_, m)| m.name).collect();
        assert_eq!(names.len(), Command::ALL.len(), "duplicate name in ALL");
    }

    #[test]
    fn for_platform_splits_tui_from_gateways() {
        // TUI sees everything.
        let tui: Vec<&str> = Command::for_platform(Platform::Tui)
            .map(|m| m.name)
            .collect();
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
        let tg: Vec<&str> = Command::for_platform(Platform::Telegram)
            .map(|m| m.name)
            .collect();
        assert_eq!(tg, vec!["reset", "new", "compact", "status"]);

        // Every command lists at least one platform.
        for (_, m) in Command::ALL {
            assert!(!m.platforms.is_empty(), "ALL entry '{}' has no platform", m.name);
        }
    }
}
