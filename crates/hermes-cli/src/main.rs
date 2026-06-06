//! Hermes CLI — interactive REPL for the Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./hermes.toml`) and launches the REPL shell.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use hermes_agent::{AIAgent, HermesConfig, SessionContext};

mod cli_render;
mod config_path;
mod ctrl_c;
mod repl;

#[cfg(test)]
pub(crate) use cli_render::{tool_emoji, truncate_str};
pub(crate) use config_path::resolve_config_path;
use repl::run_repl;

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

    /// Working directory for the session (defaults to the process's cwd).
    #[arg(long)]
    cwd: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let tokio_rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    tokio_rt.block_on(async { dispatch(args).await })
}

async fn dispatch(args: Args) -> anyhow::Result<()> {
    let config_path = resolve_config_path(args.config.as_deref())?;
    let config = HermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    let working_dir = match args.cwd {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let session = SessionContext {
        working_dir,
        session_id: "cli".into(),
    };

    let agent = AIAgent::from_config(config)
        .with_context(|| format!("failed to build agent from {}", config_path.display()))?;

    run_repl(agent, &session).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::path::Path;

    /// Serializes tests that mutate process-wide state (`HOME`). `cargo
    /// test` runs `#[test]` functions in parallel by default; without
    /// this, two tests setting `HOME` concurrently can observe each
    /// other's value. Locking the same `Mutex` (poisoning on panic is
    /// fine — we have no other invariants to protect) keeps them
    /// sequential. The integration tests in `tests/cli_smoke.rs` do
    /// not need this because each spawns a child process with HOME
    /// passed via `.env()`, never touching the test process's env.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Returns `(temp_home, cwd_dir)` with neither containing a config
    /// file. Each call produces a unique base directory (pid + nanos);
    /// leftover dirs under the system temp dir are harmless.
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

    /// RAII guard that swaps the process cwd for the duration of a test and
    /// restores the previous cwd on drop, even if assertions panic.
    /// `cargo test` runs `#[test]` functions on separate threads; since
    /// cwd is process-wide, the `ENV_LOCK` mutex below must serialize any
    /// test that calls `chdir`. (See `make_empty_dirs` for why leftover
    /// dirs are harmless.)
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

        // SAFETY: `ENV_LOCK` (above) serializes env mutations across our
        // tests, and `dispatch` / `resolve_config_path` run on this same
        // thread inside the test process — no other thread reads HOME while
        // we hold the lock.
        unsafe { std::env::set_var("HOME", &home); }
        let result = resolve_config_path(None);
        // SAFETY: see `set_var` call above; the lock is still held and the
        // remove only affects HOME in this test process.
        unsafe { std::env::remove_var("HOME"); }

        // `resolve_config_path` returns the relative `hermes.toml` for the
        // cwd fallback, so we assert by reading the file it resolved —
        // the only `hermes.toml` under cwd is the one we just wrote.
        let resolved = result.expect("should resolve to ./hermes.toml");
        let contents = std::fs::read_to_string(&resolved)
            .expect("resolved path should be readable");
        assert!(contents.contains("echo"), "resolved the wrong file: {contents}");
    }

    #[test]
    fn resolve_errors_with_message_naming_all_tried_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (home, cwd) = make_empty_dirs();
        let _cwd_guard = CwdGuard::enter(&cwd);
        // SAFETY: `ENV_LOCK` (above) serializes env mutations across our
        // tests, and `dispatch` / `resolve_config_path` run on this same
        // thread inside the test process — no other thread reads HOME while
        // we hold the lock.
        unsafe { std::env::set_var("HOME", &home); }
        let result = resolve_config_path(None);
        // SAFETY: see `set_var` call above; the lock is still held and the
        // remove only affects HOME in this test process.
        unsafe { std::env::remove_var("HOME"); }

        let err = result.unwrap_err().to_string();
        assert!(err.contains("no hermes config found"), "{err}");
        assert!(err.contains(".perry_hermes"), "{err}");
        assert!(err.contains("hermes.toml"), "{err}");
        assert!(err.contains("examples/config/hermes.toml"), "{err}");
    }

    #[test]
    fn truncate_str_adds_ellipsis_only_when_needed() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello…");
    }

    #[test]
    fn tool_emoji_maps_terminal_and_files() {
        assert_eq!(tool_emoji("terminal"), "⚡");
        assert_eq!(tool_emoji("write_file"), "📄");
        assert_eq!(tool_emoji("unknown_tool"), "🔧");
    }
}
