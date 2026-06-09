use std::sync::Arc;

use async_trait::async_trait;
use perry_hermes_core::commands::Command;
use perry_hermes_core::Platform;
use teloxide::prelude::*;
use teloxide::types::{BotCommand, ChatAction, ChatKind};
use tracing::{info, warn};

use crate::adapter::PlatformAdapter;
use crate::event::{ChatType, GatewayEvent};
use crate::runner::GatewayRunner;

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
        // Name + description come from `Command::ALL` — single source of truth
        // shared with every other platform; this adapter doesn't need to
        // know which specific names belong to the Telegram subset.
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

                match gateway.handle_event(event).await {
                    Ok(crate::runner::GatewayResponse::Reply(text)) => {
                        if let Err(e) = bot.send_message(chat_id, &text).await {
                            warn!(error = %e, "failed to send Telegram reply");
                        }
                    }
                    Ok(crate::runner::GatewayResponse::Ignored) => {}
                    Err(e) => {
                        warn!(error = %e, "gateway error");
                        let _ = bot.send_message(chat_id, format!("Error: {e}")).await;
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
