//! QQ Bot v2 platform adapter.
//!
//! Uses [`qq_bot_rs`] for the WebSocket transport (handshake, heartbeat,
//! resume, auto-reconnect) and bridges the typed events into the gateway's
//! [`GatewayRunner::handle_event`].

use std::sync::Arc;

use async_trait::async_trait;
use qq_bot_rs::types::message::{C2cMessage, GroupMessage, OutgoingMessage};

use crate::adapter::PlatformAdapter;
use crate::qqbot::QqBotConfig;
use crate::runner::GatewayRunner;

/// Convert a raw intent `u32` bitmask from the config into the lib's
/// typed `Intents`. `0` falls back to `PUBLIC_MESSAGES` (C2C + group @).
fn build_intents(bits: u32) -> qq_bot_rs::Intents {
    if bits == 0 {
        qq_bot_rs::Intents::PUBLIC_MESSAGES
    } else {
        qq_bot_rs::Intents::from_bits_truncate(bits)
    }
}

/// Adapter that runs a QQ Bot v2 client and dispatches inbound events.
pub struct QQBotAdapter {
    config: QqBotConfig,
}

impl QQBotAdapter {
    pub fn new(config: QqBotConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl PlatformAdapter for QQBotAdapter {
    fn name(&self) -> &str {
        "qqbot"
    }

    async fn run(&self, gateway: Arc<GatewayRunner>) -> anyhow::Result<()> {
        let (app_id, app_secret) = self.config.resolve()?;
        let intents = build_intents(self.config.intents);

        let bridge = QqEventBridge {
            gateway: Arc::clone(&gateway),
        };

        // Sandbox mode routes REST + WS to the QQ sandbox environment.
        // The lib's BotBuilder distinguishes via .sandbox(true).
        let bot = qq_bot_rs::Bot::builder()
            .sandbox(self.config.sandbox)
            .build(qq_bot_rs::Credentials::new(app_id, app_secret));

        let client = qq_bot_rs::Client::builder()
            .bot(bot)
            .intents(intents)
            .handler(bridge)
            .build()?;

        // lib's run() blocks until the WS closes (transient close codes
        // are auto-recovered internally; fatal codes 4914/4915 exit).
        // For MVP we don't try to cancel mid-run; lib's reconnect handles
        // transient drops. disconnect() is a no-op.
        client.run().await.map_err(|e| anyhow::anyhow!("qqbot client exited: {e}"))
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        // No-op for MVP. lib's run() will exit on the next fatal WS close.
        // A future iteration can add a oneshot::Sender<()> shutdown channel
        // plumbed into QqEventBridge if mid-run cancel is needed.
        Ok(())
    }
}

/// Bridges `qq_bot_rs::EventHandler` callbacks into the gateway.
struct QqEventBridge {
    gateway: Arc<GatewayRunner>,
}

#[async_trait]
impl qq_bot_rs::EventHandler for QqEventBridge {
    async fn on_c2c_message_create(&self, bot: &qq_bot_rs::Bot, msg: C2cMessage) {
        let Some(ev) = super::events::c2c_to_event(&msg) else {
            return;
        };
        let user_openid = msg.author.user_openid.clone();
        super::events::handle_reply(&self.gateway, &ev, move |text| async move {
            let reply = OutgoingMessage::text(text);
            bot.post_c2c_message(&user_openid, &reply)
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .await;
    }

    async fn on_group_at_message_create(&self, bot: &qq_bot_rs::Bot, msg: GroupMessage) {
        let Some(ev) = super::events::group_to_event(&msg) else {
            return;
        };
        let group_openid = msg.group_openid.clone();
        super::events::handle_reply(&self.gateway, &ev, move |text| async move {
            let reply = OutgoingMessage::text(text);
            bot.post_group_message(&group_openid, &reply)
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .await;
    }
}
