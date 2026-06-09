# `perry-hermes-gateway`

Platform gateway for Perry Hermes — session management and multi-platform
adapter dispatch.

See the [workspace README](../../README.md) for the overall project.

## Adapters

### Telegram

```rust,no_run
use std::sync::Arc;
use perry_hermes_gateway::{GatewayConfig, GatewayRunner, telegram::TelegramAdapter};

# async fn example(agent: Arc<perry_hermes_agent::AgentLoop>) -> anyhow::Result<()> {
let config = GatewayConfig::default();
let runner = GatewayRunner::new(agent, config);
let telegram = Arc::new(TelegramAdapter::new("BOT_TOKEN"));
// runner.run(vec![telegram]).await?;
# Ok(())
# }
```

Set the `TELEGRAM_BOT_TOKEN` environment variable (or pass the token
directly to `TelegramAdapter::new`).

### QQBot

```rust,no_run
use std::sync::Arc;
use perry_hermes_gateway::{GatewayConfig, GatewayRunner, QQBotAdapter, QqBotConfig};

# async fn example(agent: Arc<perry_hermes_agent::AgentLoop>) -> anyhow::Result<()> {
let config = GatewayConfig::default();
let runner = GatewayRunner::new(agent, config);
let qqbot = Arc::new(QQBotAdapter::new(QqBotConfig::default()));
// runner.run(vec![qqbot]).await?;
# Ok(())
# }
```

Set the `QQ_BOT_APP_ID` and `QQ_BOT_APP_SECRET` environment variables
before running. To use the QQ sandbox environment instead, set
`QqBotConfig { sandbox: true, ..QqBotConfig::default() }`.

The adapter uses the `qq-bot-rs` WebSocket transport — no public HTTPS
endpoint is required. It subscribes to `Intents::PUBLIC_MESSAGES` by
default (C2C private messages + group @-mentions).

## Building

```sh
cargo build -p perry-hermes-gateway
```

## Testing

```sh
cargo test -p perry-hermes-gateway
```
