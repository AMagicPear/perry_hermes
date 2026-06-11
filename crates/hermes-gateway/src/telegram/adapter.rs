use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::Platform;
use perry_hermes_core::commands::Command;
use perry_hermes_core::message::ToolCall;
use teloxide::prelude::*;
use teloxide::types::{BotCommand, ChatAction, ChatKind, ParseMode};
use tracing::{info, warn};

use crate::adapter::PlatformAdapter;
use crate::event::{ChatType, GatewayEvent};
use crate::handler::GatewayEventHandler;
use crate::runner::GatewayRunner;

/// Send a message with MarkdownV2 formatting; fall back to plain text on failure.
async fn send_markdown(bot: &Bot, chat_id: ChatId, text: &str) {
    let formatted = telegram_markdown_v2::convert_with_strategy(
        text,
        telegram_markdown_v2::UnsupportedTagsStrategy::Escape,
    )
    .unwrap_or_else(|_| text.to_string());
    if let Err(e) = bot
        .send_message(chat_id, &formatted)
        .parse_mode(ParseMode::MarkdownV2)
        .await
    {
        warn!(error = %e, "MarkdownV2 send failed, falling back to plain text");
        if let Err(e2) = bot.send_message(chat_id, text).await {
            warn!(error = %e2, "plain text fallback also failed");
        }
    }
}

// ── Streaming event handler ─────────────────────────────────────────

/// Streams agent output to a Telegram chat, sending each content
/// segment as a separate message.
///
/// Content is buffered in `content_buffer` and flushed at tool call
/// boundaries (`on_tool_started`) and turn completion
/// (`on_turn_completed`). Each flush sends one Telegram message.
struct TelegramEventHandler {
    bot: Bot,
    chat_id: ChatId,
    content_buffer: String,
}

impl TelegramEventHandler {
    fn new(bot: Bot, chat_id: ChatId) -> Self {
        Self {
            bot,
            chat_id,
            content_buffer: String::new(),
        }
    }

    /// Flush accumulated content as a Telegram message. No-op if buffer
    /// is empty.
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.content_buffer);
        if text.trim().is_empty() {
            return;
        }
        let bot = self.bot.clone();
        let chat_id = self.chat_id;
        tokio::spawn(async move {
            send_markdown(&bot, chat_id, &text).await;
        });
    }
}

impl GatewayEventHandler for TelegramEventHandler {
    fn on_content_delta(&mut self, text: &str) {
        self.content_buffer.push_str(text);
    }

    fn on_tool_started(&mut self, _call: &ToolCall, _iteration: u32) {
        // Flush content accumulated before this tool call.
        self.flush();
    }

    fn on_error(&mut self, error: &str) {
        self.content_buffer.push_str(&format!("⚠ Error: {error}"));
    }

    fn on_turn_completed(&mut self) {
        // Flush any remaining content from the final iteration.
        self.flush();
    }
}

// ── Adapter ─────────────────────────────────────────────────────────

/// Telegram platform adapter using teloxide long-polling.
pub struct TelegramAdapter {
    bot: Bot,
}

impl TelegramAdapter {
    pub fn new(bot_token: &str) -> Self {
        Self {
            bot: Bot::new(bot_token),
        }
    }

    /// Convert a teloxide `Message` into a `GatewayEvent`.
    fn message_to_event(msg: &Message) -> Option<GatewayEvent> {
        let text = msg.text()?;
        if text.is_empty() {
            return None;
        }

        let chat_id = msg.chat.id.to_string();
        let (chat_type, thread_id) = match &msg.chat.kind {
            ChatKind::Private(_) => (ChatType::Dm, None),
            ChatKind::Public(_) => {
                let tid = msg.thread_id.map(|id| id.0.0.to_string());
                if msg.chat.is_channel() {
                    (ChatType::Channel, tid)
                } else if tid.is_some() {
                    (ChatType::Thread, tid)
                } else {
                    (ChatType::Group, tid)
                }
            }
        };

        let user_id = msg
            .from
            .as_ref()
            .map(|u| u.id.0.to_string())
            .unwrap_or_default();

        let user_name = msg.from.as_ref().and_then(|u| {
            if !u.first_name.is_empty() {
                Some(u.first_name.clone())
            } else {
                u.username.clone().map(|n| format!("@{n}"))
            }
        });

        Some(GatewayEvent {
            platform: Platform::Telegram,
            chat_id,
            chat_type,
            user_id,
            user_name,
            thread_id,
            text: text.to_string(),
            message_id: Some(msg.id.to_string()),
            timestamp: msg.date,
        })
    }
}

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    fn name(&self) -> &str {
        Platform::Telegram.as_str()
    }

    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()> {
        info!("Telegram adapter starting (long-poll)");

        // Register commands with Telegram so users see them in the "/" menu.
        let commands: Vec<BotCommand> = Command::for_platform(Platform::Telegram)
            .map(|m| BotCommand::new(m.name, m.description))
            .collect();
        if let Err(e) = self.bot.set_my_commands(commands).send().await {
            warn!(error = %e, "failed to register Telegram commands");
        } else {
            info!("registered Telegram bot commands");
        }

        let bot = self.bot.clone();

        teloxide::repl(bot, move |bot: Bot, msg: Message| {
            let gateway = Arc::clone(&gateway);
            async move {
                let Some(event) = TelegramAdapter::message_to_event(&msg) else {
                    return respond(());
                };

                let chat_id = msg.chat.id;

                // Send typing indicator
                let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

                // Create a streaming handler for this message.
                // Content segments are sent as separate Telegram messages
                // at tool call boundaries and turn completion.
                let mut handler = TelegramEventHandler::new(bot.clone(), chat_id);

                match gateway.handle_event(event, &mut handler).await {
                    Ok(crate::runner::GatewayResponse::CommandReply(text)) => {
                        send_markdown(&bot, chat_id, &text).await;
                    }
                    Ok(crate::runner::GatewayResponse::Ignored) => {}
                    Err(e) => {
                        warn!(error = %e, "gateway error");
                        let _ = bot.send_message(chat_id, format!("⚠ Error: {e}")).await;
                    }
                }

                respond(())
            }
        })
        .await;

        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        info!("Telegram adapter disconnecting");
        Ok(())
    }
}
