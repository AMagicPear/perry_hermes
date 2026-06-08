/// Telegram bot command constants.
///
/// These are the slash commands that users can send to the bot.
/// They are checked in `GatewayRunner::handle_event()` before
/// passing messages to the agent.
pub const CMD_RESET: &str = "/reset";
pub const CMD_NEW: &str = "/new";
pub const CMD_COMPACT: &str = "/compact";
pub const CMD_STATUS: &str = "/status";
