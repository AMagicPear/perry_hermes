//! Perry Hermes CLI — interactive TUI for the Perry Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./perry_hermes.toml`) and launches the ratatui TUI.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

use perry_hermes_agent::{AIAgent, PerryHermesConfig};

mod config_path;

#[derive(Parser)]
#[command(
    name = "perry-hermes",
    version,
    about = "Perry Hermes — AI agent with tool use",
    long_about = None
)]
struct Args {
    /// Path to Perry Hermes TOML config. If omitted, the CLI looks in
    /// `~/.perry_hermes/config.toml` then `./perry_hermes.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Provider name to use for this run, overriding `[agent].default_provider`.
    #[arg(long)]
    provider: Option<String>,
    /// Model name to use for this run, overriding `[agent].default_model`.
    #[arg(long)]
    model: Option<String>,
    /// Subcommand. Defaults to the interactive TUI.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the interactive TUI (default when no subcommand is given).
    Tui,
    /// Start the platform gateway: connect all configured adapters.
    Gateway,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = config_path::resolve_config_path(args.config.as_deref())?;
    let config = PerryHermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;
    let config = apply_cli_provider_overrides(config, &args);

    match args.command.unwrap_or(Command::Tui) {
        Command::Tui => run_tui(config, &config_path).await,
        Command::Gateway => run_gateway(config, &config_path).await,
    }
}

async fn run_tui(config: PerryHermesConfig, config_path: &Path) -> anyhow::Result<()> {
    let selected_provider = config
        .resolve_provider()
        .with_context(|| format!("failed to resolve provider from {}", config_path.display()))?;
    let provider_name = selected_provider.name.clone();
    let model_name = selected_provider.model.clone();

    let max_iterations = config.agent.max_iterations.unwrap_or(10);
    let context_window_size = Some(selected_provider.context_window_size);

    let agent = Arc::new(
        AIAgent::from_config(config)
            .with_context(|| format!("failed to build agent from {}", config_path.display()))?,
    );

    let cancel = tokio_util::sync::CancellationToken::new();

    perry_hermes_cli::tui::run(
        agent,
        cancel,
        provider_name,
        model_name,
        max_iterations,
        context_window_size,
    )
    .await?;
    Ok(())
}

async fn run_gateway(config: PerryHermesConfig, config_path: &Path) -> anyhow::Result<()> {
    use perry_hermes_gateway::{
        GatewayConfig, GatewayRunner, QQBotAdapter, QqBotConfig, telegram::TelegramAdapter,
    };

    let agent = Arc::new(
        AIAgent::from_config(config)
            .with_context(|| format!("failed to build agent from {}", config_path.display()))?,
    );

    let mut gateway_config = GatewayConfig::default();

    // Load allowed users from env
    if let Ok(allowed) = std::env::var("TELEGRAM_ALLOWED_USERS") {
        let users: HashSet<String> = allowed
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !users.is_empty() {
            gateway_config
                .allowed_users
                .insert("telegram".into(), users);
        }
    }

    let runner = GatewayRunner::new(agent, gateway_config);

    // Discover configured platform adapters
    let mut adapters: Vec<Arc<dyn perry_hermes_gateway::PlatformAdapter>> = Vec::new();

    if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN")
        && !token.is_empty()
    {
        adapters.push(Arc::new(TelegramAdapter::new(&token)));
        eprintln!("Telegram adapter enabled");
    }

    // QQ Bot — credentials from env by default. Set both
    // QQ_BOT_APP_ID and QQ_BOT_APP_SECRET to enable.
    let qq_app_id = std::env::var("QQ_BOT_APP_ID").ok();
    let qq_app_secret = std::env::var("QQ_BOT_APP_SECRET").ok();
    if qq_app_id.as_deref().is_some_and(|s| !s.is_empty())
        && qq_app_secret.as_deref().is_some_and(|s| !s.is_empty())
    {
        let qqbot_cfg = QqBotConfig {
            app_id: qq_app_id,
            app_secret: qq_app_secret,
            sandbox: std::env::var("QQ_BOT_SANDBOX")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            ..QqBotConfig::default()
        };
        adapters.push(Arc::new(QQBotAdapter::new(qqbot_cfg)));
        eprintln!("QQBot adapter enabled");
    }

    if adapters.is_empty() {
        anyhow::bail!(
            "No platform adapters configured. Set TELEGRAM_BOT_TOKEN to enable Telegram, \
             or QQ_BOT_APP_ID + QQ_BOT_APP_SECRET to enable QQ Bot."
        );
    }

    runner.run(adapters).await
}

fn apply_cli_provider_overrides(mut config: PerryHermesConfig, args: &Args) -> PerryHermesConfig {
    if let Some(provider) = &args.provider {
        config.agent.default_provider = provider.clone();
    }
    if let Some(model) = &args.model {
        config.agent.default_model = model.clone();
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_path::resolve_config_path;
    use std::path::Path;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_empty_dirs() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "perry-hermes-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = base.join("home");
        let cwd = base.join("cwd");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        (home, cwd)
    }

    #[test]
    fn resolve_explicit_path_must_exist() {
        let _guard = ENV_LOCK.lock().unwrap();
        let result = resolve_config_path(Some(Path::new("/does/not/exist.toml")));
        let err = result.unwrap_err().to_string();
        assert!(err.contains("/does/not/exist.toml"), "{err}");
    }

    struct CwdGuard {
        previous: PathBuf,
    }
    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { previous }
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    #[test]
    fn resolve_picks_cwd_perry_hermes_toml_when_no_home_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, cwd) = make_empty_dirs();
        let _cwd_guard = CwdGuard::enter(&cwd);
        let config_path = cwd.join("perry_hermes.toml");
        std::fs::write(
            &config_path,
            r#"
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("HOME", &home);
        }
        let result = resolve_config_path(None);
        unsafe {
            std::env::remove_var("HOME");
        }

        let resolved = result.expect("should resolve to ./perry_hermes.toml");
        let contents =
            std::fs::read_to_string(&resolved).expect("resolved path should be readable");
        assert!(
            contents.contains("echo"),
            "resolved the wrong file: {contents}"
        );
    }

    #[test]
    fn resolve_errors_with_message_naming_all_tried_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, cwd) = make_empty_dirs();
        let _cwd_guard = CwdGuard::enter(&cwd);
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let result = resolve_config_path(None);
        unsafe {
            std::env::remove_var("HOME");
        }

        let err = result.unwrap_err().to_string();
        assert!(err.contains("no Perry Hermes config found"), "{err}");
        assert!(err.contains(".perry_hermes"), "{err}");
        assert!(err.contains("perry_hermes.toml"), "{err}");
    }

    #[test]
    fn cli_provider_and_model_override_config_defaults() {
        let config = PerryHermesConfig {
            providers: vec![perry_hermes_agent::ProviderConfig {
                name: "minimax".into(),
                kind: perry_hermes_agent::ProviderKind::Anthropic,
                api_key_env: Some("MINIMAX_API_KEY".into()),
                models: vec![
                    perry_hermes_agent::ModelConfig {
                        name: "MiniMax-M3".into(),
                        context_window_size: 1_000_000,
                    },
                    perry_hermes_agent::ModelConfig {
                        name: "MiniMax-M2.7".into(),
                        context_window_size: 204_800,
                    },
                ],
                base_url: Some("https://api.minimaxi.com/anthropic/v1".into()),
                api_key_header: None,
                thinking: None,
            }],
            agent: perry_hermes_agent::AgentConfig {
                default_provider: "minimax".into(),
                default_model: "MiniMax-M3".into(),
                ..Default::default()
            },
            gateway: perry_hermes_agent::GatewayTomlConfig::default(),
        };
        let args = Args {
            config: None,
            provider: Some("minimax".into()),
            model: Some("MiniMax-M2.7".into()),
            command: None,
        };

        let config = apply_cli_provider_overrides(config, &args);
        let selected = config.resolve_provider().unwrap();

        assert_eq!(selected.name, "minimax");
        assert_eq!(selected.model, "MiniMax-M2.7");
        assert_eq!(selected.context_window_size, 204_800);
    }
}
