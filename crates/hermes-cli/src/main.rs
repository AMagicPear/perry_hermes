//! Perry Hermes CLI — interactive TUI for the Perry Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./perry_hermes.toml`) and launches the ratatui TUI.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

use perry_hermes_agent::{AgentLoop, PerryHermesConfig};

mod config;

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
    /// Manage the platform gateway.
    Gateway {
        #[command(subcommand)]
        subcommand: GatewayCommand,
    },
}

#[derive(Subcommand)]
enum GatewayCommand {
    /// Run the gateway in the foreground (blocking).
    Run,
    /// Register and start the gateway as a system service.
    Start,
    /// Stop the gateway system service.
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = config::resolve_config_path(args.config.as_deref())?;
    let config = PerryHermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;
    let config = config::apply_cli_provider_overrides(config, &args);

    match args.command.unwrap_or(Command::Tui) {
        Command::Tui => run_tui(config, &config_path).await,
        Command::Gateway { subcommand } => match subcommand {
            GatewayCommand::Run => run_gateway(config, &config_path).await,
            GatewayCommand::Start => gateway_start(),
            GatewayCommand::Stop => gateway_stop(),
        },
    }
}

async fn run_tui(config: PerryHermesConfig, config_path: &Path) -> anyhow::Result<()> {
    let selected_provider = config
        .resolve_provider()
        .with_context(|| format!("failed to resolve provider from {}", config_path.display()))?;
    let provider_name = selected_provider.name.clone();
    let model_name = selected_provider.model.clone();

    let max_iterations = config.agent.max_iterations;
    let context_window_size = Some(selected_provider.context_window_size);

    let agent = Arc::new(
        AgentLoop::from_config(config)
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
        AgentLoop::from_config(config)
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

/// Environment variables to capture when installing the gateway service.
const GATEWAY_ENV_VARS: &[&str] = &[
    "TELEGRAM_BOT_TOKEN",
    "QQ_BOT_APP_ID",
    "QQ_BOT_APP_SECRET",
    "QQ_BOT_SANDBOX",
    "TELEGRAM_ALLOWED_USERS",
    "MINIMAX_API_KEY",
    "MIMO_API_KEY",
    "DEEPSEEK_API_KEY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "PERRY_HERMES_HOME",
];

/// Write captured env vars to `$PERRY_HERMES_HOME/gateway.env` as KEY=VALUE lines.
fn write_gateway_env_file() -> anyhow::Result<PathBuf> {
    let env_path = perry_hermes_core::home::resolve_gateway_env_path()
        .context("cannot resolve Perry Hermes home directory")?;
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut lines = Vec::new();
    for &key in GATEWAY_ENV_VARS {
        if let Ok(val) = std::env::var(key)
            && !val.is_empty()
        {
            lines.push(format!("{key}={val}"));
        }
    }
    std::fs::write(&env_path, lines.join("\n") + "\n")
        .with_context(|| format!("failed to write {}", env_path.display()))?;
    Ok(env_path)
}

fn gateway_binary_path() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("failed to determine current executable path")
}

// ── macOS (launchd) ──────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn gateway_start() -> anyhow::Result<()> {
    let env_path = write_gateway_env_file()?;
    let exe = gateway_binary_path()?;
    let log_dir = perry_hermes_core::home::resolve_logs_dir()
        .context("cannot resolve Perry Hermes home directory")?;
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create {}", log_dir.display()))?;

    let stdout_path = log_dir.join("gateway-stdout.log");
    let stderr_path = log_dir.join("gateway-stderr.log");

    // Read env vars back for embedding in plist
    let env_entries: Vec<String> = GATEWAY_ENV_VARS
        .iter()
        .filter_map(|&key| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|val| format!("        <key>{key}</key>\n        <string>{val}</string>"))
        })
        .collect();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.perry-hermes.gateway</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>gateway</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env_entries}
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        env_entries = env_entries.join("\n"),
        stdout = stdout_path.display(),
        stderr = stderr_path.display(),
    );

    let user_home =
        perry_hermes_core::home::user_home_dir().context("cannot determine user home directory")?;
    let plist_dir = PathBuf::from(&user_home)
        .join("Library")
        .join("LaunchAgents");
    std::fs::create_dir_all(&plist_dir)
        .with_context(|| format!("failed to create {}", plist_dir.display()))?;
    let plist_path = plist_dir.join("com.perry-hermes.gateway.plist");
    std::fs::write(&plist_path, &plist)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;

    // Bootstrap (load + start) the service
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run id -u")?;
    let uid = String::from_utf8(uid.stdout).context("invalid uid output")?;
    let uid = uid.trim();
    let target = format!("gui/{uid}");
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &target, &plist_path.to_string_lossy()])
        .status()
        .context("failed to run launchctl bootstrap")?;

    if status.success() {
        println!("Gateway service started.");
        println!("  plist: {}", plist_path.display());
        println!("  env:   {}", env_path.display());
        println!("  logs:  {}", log_dir.display());
    } else {
        // bootstrap returns non-zero if already loaded; try kickstart instead
        let status2 = std::process::Command::new("launchctl")
            .args([
                "kickstart",
                "-k",
                &format!("{target}/com.perry-hermes.gateway"),
            ])
            .status()
            .context("failed to run launchctl kickstart")?;
        if status2.success() {
            println!("Gateway service restarted.");
            println!("  plist: {}", plist_path.display());
        } else {
            anyhow::bail!("launchctl bootstrap/kickstart failed");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn gateway_stop() -> anyhow::Result<()> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run id -u")?;
    let uid = String::from_utf8(uid.stdout).context("invalid uid output")?;
    let uid = uid.trim();
    let target = format!("gui/{uid}");
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{target}/com.perry-hermes.gateway")])
        .status()
        .context("failed to run launchctl bootout")?;

    if status.success() {
        println!("Gateway service stopped.");
    } else {
        eprintln!("Service was not running (or already stopped).");
    }
    Ok(())
}

// ── Windows (schtasks) ──────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn gateway_start() -> anyhow::Result<()> {
    let env_path = write_gateway_env_file()?;
    let exe = gateway_binary_path()?;
    let log_dir = perry_hermes_core::home::resolve_logs_dir()
        .context("cannot resolve Perry Hermes home directory")?;
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create {}", log_dir.display()))?;

    let stdout_path = log_dir.join("gateway-stdout.log");
    let stderr_path = log_dir.join("gateway-stderr.log");

    // Write a launcher batch script that loads env vars then runs the gateway
    let launcher_path = perry_hermes_core::home::resolve_gateway_launcher_path()
        .context("cannot resolve Perry Hermes home directory")?;
    let mut script = String::from("@echo off\n");
    // Load env vars from the env file
    script.push_str(&format!(
        "for /f \"usebackq tokens=* delims=\" %%a in (\"{}\") do set \"%%a\"\n",
        env_path.display()
    ));
    script.push_str(&format!(
        "\"{}\" gateway run 1>\"{}\" 2>\"{}\"\n",
        exe.display(),
        stdout_path.display(),
        stderr_path.display(),
    ));
    std::fs::write(&launcher_path, &script)
        .with_context(|| format!("failed to write {}", launcher_path.display()))?;

    // Register a scheduled task that runs at user logon
    let task_name = "PerryHermesGateway";
    let status = std::process::Command::new("schtasks")
        .args([
            "/create",
            "/tn",
            task_name,
            "/tr",
            &launcher_path.to_string_lossy(),
            "/sc",
            "onlogon",
            "/f",
        ])
        .status()
        .context("failed to run schtasks /create")?;

    if !status.success() {
        anyhow::bail!("schtasks /create failed");
    }

    // Start immediately
    let start_status = std::process::Command::new("schtasks")
        .args(["/run", "/tn", task_name])
        .status()
        .context("failed to run schtasks /run")?;

    if start_status.success() {
        println!("Gateway service started.");
        println!("  task:   {}", task_name);
        println!("  env:    {}", env_path.display());
        println!("  logs:   {}", log_dir.display());
    } else {
        eprintln!(
            "Warning: schtasks /run failed, but task was created. It will start at next logon."
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn gateway_stop() -> anyhow::Result<()> {
    let task_name = "PerryHermesGateway";
    // End the task (kills the process tree)
    let status = std::process::Command::new("schtasks")
        .args(["/end", "/tn", task_name])
        .status()
        .context("failed to run schtasks /end")?;

    if status.success() {
        println!("Gateway service stopped.");
    } else {
        eprintln!("Service was not running (or already stopped).");
    }
    Ok(())
}

// ── Linux (systemd) ──────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn gateway_start() -> anyhow::Result<()> {
    let env_path = write_gateway_env_file()?;
    let exe = gateway_binary_path()?;
    let home =
        perry_hermes_core::home::user_home_dir().context("cannot determine user home directory")?;

    let service = format!(
        r#"[Unit]
Description=Perry Hermes Gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe} gateway run
EnvironmentFile={env_path}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        exe = exe.display(),
        env_path = env_path.display(),
    );

    let unit_dir = PathBuf::from(&home)
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&unit_dir)
        .with_context(|| format!("failed to create {}", unit_dir.display()))?;
    let unit_path = unit_dir.join("perry-hermes-gateway.service");
    std::fs::write(&unit_path, &service)
        .with_context(|| format!("failed to write {}", unit_path.display()))?;

    // daemon-reload + enable + start
    let run = |args: &[&str]| -> anyhow::Result<()> {
        let status = std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .status()
            .with_context(|| format!("failed to run systemctl --user {}", args.join(" ")))?;
        if !status.success() {
            anyhow::bail!("systemctl --user {} failed", args.join(" "));
        }
        Ok(())
    };

    run(&["daemon-reload"])?;
    run(&["enable", "perry-hermes-gateway"])?;
    run(&["start", "perry-hermes-gateway"])?;

    println!("Gateway service started.");
    println!("  unit: {}", unit_path.display());
    println!("  env:  {}", env_path.display());
    println!("  logs: journalctl --user -u perry-hermes-gateway -f");
    Ok(())
}

#[cfg(target_os = "linux")]
fn gateway_stop() -> anyhow::Result<()> {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "stop", "perry-hermes-gateway"])
        .status()
        .context("failed to run systemctl --user stop")?;

    if status.success() {
        println!("Gateway service stopped.");
    } else {
        eprintln!("Service was not running (or already stopped).");
    }
    Ok(())
}
