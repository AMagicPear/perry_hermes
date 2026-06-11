use std::path::{Path, PathBuf};

use anyhow::bail;
use perry_hermes_agent::PerryHermesConfig;

pub(crate) fn resolve_config_path(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--config {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }

    let mut tried = Vec::new();
    if let Some(p) = perry_hermes_core::home::resolve_config_path() {
        tried.push(p.clone());
        if p.exists() {
            return Ok(p);
        }
    }
    let cwd_default = PathBuf::from("perry_hermes.toml");
    tried.push(cwd_default.clone());
    if cwd_default.exists() {
        return Ok(cwd_default);
    }

    let mut msg = String::from("no Perry Hermes config found. Looked for:\n");
    for p in &tried {
        msg.push_str(&format!("  - {}\n", p.display()));
    }
    msg.push_str(
        "Pass --config <path> or create one of these. See examples/config/perry_hermes.toml for a starter.",
    );
    bail!(msg);
}

pub(crate) fn apply_cli_provider_overrides(
    mut config: PerryHermesConfig,
    args: &crate::Args,
) -> PerryHermesConfig {
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
    use perry_hermes_agent::PerryHermesConfig;

    use super::*;
    use crate::config::resolve_config_path;
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
        let args = crate::Args {
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
