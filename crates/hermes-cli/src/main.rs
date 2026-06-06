//! Hermes CLI — interactive TUI for the Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./hermes.toml`) and launches the ratatui TUI.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;

use hermes_agent::{AIAgent, HermesConfig};

mod config_path;

#[derive(Parser)]
#[command(
    name = "hermes",
    version,
    about = "Hermes — AI agent with tool use",
    long_about = None
)]
struct Args {
    /// Path to HermesConfig TOML. If omitted, the CLI looks in
    /// `~/.perry_hermes/config.toml` then `./hermes.toml`.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = config_path::resolve_config_path(args.config.as_deref())?;
    let config = HermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    let provider_name = provider_name(&config.provider).to_string();
    let model_name = config.provider.model.clone().unwrap_or_else(|| "?".to_string());

    let agent = Arc::new(
        AIAgent::from_config(config)
            .with_context(|| format!("failed to build agent from {}", config_path.display()))?,
    );

    let cancel = tokio_util::sync::CancellationToken::new();

    hermes_cli::tui::run(agent, cancel, provider_name, model_name).await?;
    Ok(())
}

fn provider_name(config: &hermes_agent::ProviderConfig) -> &'static str {
    match config.kind {
        hermes_agent::ProviderKind::Echo => "echo",
        hermes_agent::ProviderKind::Openai => "openai",
        hermes_agent::ProviderKind::Anthropic => "anthropic",
    }
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
            "hermes-cli-test-{}-{}",
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
    fn resolve_picks_cwd_hermes_toml_when_no_home_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, cwd) = make_empty_dirs();
        let _cwd_guard = CwdGuard::enter(&cwd);
        let config_path = cwd.join("hermes.toml");
        std::fs::write(&config_path, "[provider]\nkind=\"echo\"\n").unwrap();

        unsafe {
            std::env::set_var("HOME", &home);
        }
        let result = resolve_config_path(None);
        unsafe {
            std::env::remove_var("HOME");
        }

        let resolved = result.expect("should resolve to ./hermes.toml");
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
        assert!(err.contains("no hermes config found"), "{err}");
        assert!(err.contains(".perry_hermes"), "{err}");
        assert!(err.contains("hermes.toml"), "{err}");
    }
}