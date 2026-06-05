//! Hermes CLI — interactive REPL for the Hermes agent.
//!
//! Reads `--config` (or falls back to `~/.perry_hermes/config.toml` then
//! `./hermes.toml`), constructs the runtime, and renders `LoopEvent`s.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

use hermes_core::error::LoopError;
use hermes_core::message::{Content, Message, Role};
use hermes_runtime::{AIAgent, HermesConfig, LoopEvent, SessionContext};
use tokio_util::sync::CancellationToken;

mod ctrl_c;
use ctrl_c::{CtrlCAction, CtrlCHandler};

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

fn resolve_config_path(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--config {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }

    let mut tried = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".perry_hermes").join("config.toml");
        tried.push(p.clone());
        if p.exists() {
            return Ok(p);
        }
    }
    let cwd_default = PathBuf::from("hermes.toml");
    tried.push(cwd_default.clone());
    if cwd_default.exists() {
        return Ok(cwd_default);
    }

    let mut msg = String::from("no hermes config found. Looked for:\n");
    for p in &tried {
        msg.push_str(&format!("  - {}\n", p.display()));
    }
    msg.push_str("Pass --config <path> or create one of these. See crates/hermes-cli/hermes.example.toml for a starter.");
    bail!(msg);
}

async fn run_repl(agent: AIAgent, session: &SessionContext) -> anyhow::Result<()> {
    eprintln!(
        "hermes v{} — type a message, Ctrl-D to quit, Ctrl-C to cancel a turn or (when idle) quit",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut history: Vec<Message> = Vec::new();

    let ctrl_c = Arc::new(CtrlCHandler::new());
    let ctrl_c_signal = Arc::clone(&ctrl_c);
    let signal_handle = tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_ok() {
                match ctrl_c_signal.handle() {
                    CtrlCAction::Exit => {
                        eprintln!();
                        std::process::exit(0);
                    }
                    CtrlCAction::Cancel => {}
                }
            }
        }
    });

    for line in stdin.lock().lines() {
        let line = line.context("failed to read line")?;
        let line = line.trim().to_string();

        if line == "/quit" || line == "/exit" {
            break;
        }
        if line.is_empty() {
            continue;
        }

        history.push(Message {
            role: Role::User,
            content: Content::Text(line),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        });

        let cancel = CancellationToken::new();
        ctrl_c.enter_turn(cancel.clone());

        let result = agent
            .run_messages(history.clone(), session, cancel.clone(), |event| match event {
                LoopEvent::Thinking => {
                    eprint!("… ");
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallStarted { call, .. } => {
                    let preview = truncate_str(&call.arguments.to_string(), 80);
                    eprint!("\n  📦 {}({})", call.name, preview);
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallFinished { call, result } => {
                    match &result {
                        Ok(out) => {
                            let preview = truncate_str(&out.content, 160);
                            eprint!("\n  ← {} {}", tool_emoji(&call.name), preview);
                        }
                        Err(e) => {
                            eprint!("\n  ← ❌ {e}");
                        }
                    }
                    let _ = stdout.flush();
                }
                LoopEvent::AssistantMessage(_) => {
                    eprintln!();
                }
                LoopEvent::LengthLimit => eprintln!("[hit length limit]"),
                LoopEvent::IterationsExhausted => eprintln!("[max iterations]"),
                LoopEvent::Cancelled => eprintln!("[cancelled]"),
                LoopEvent::ContentDelta(s) => {
                    eprint!("{s}");
                    let _ = stdout.flush();
                }
                LoopEvent::ReasoningDelta(s) => {
                    eprint!("\x1b[2m{s}\x1b[0m");
                    let _ = stdout.flush();
                }
                LoopEvent::ToolCallPartial(_) => {}
            })
            .await;

        ctrl_c.exit_turn();

        match result {
            Ok(run_result) => {
                let _ = &run_result.final_message;
                history = run_result.messages;

                eprintln!(
                    "  [iterations={} tool_calls={} in={} out={}]",
                    run_result.metrics.iterations,
                    run_result.metrics.tool_calls,
                    run_result.metrics.input_tokens,
                    run_result.metrics.output_tokens,
                );
                eprintln!();
            }
            Err(LoopError::CancelledWith(partial)) => {
                let chars = match &partial.content {
                    Content::Text(s) => s.chars().count(),
                    Content::Parts(_) => 0,
                };
                let calls = partial.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
                eprintln!(
                    "\n  [cancelled mid-stream: {chars} chars streamed, {calls} tool call kept]"
                );
                if chars > 0 || calls > 0 {
                    history.push(partial);
                } else {
                    history.pop();
                }
                eprintln!();
            }
            Err(LoopError::Cancelled) => {
                eprintln!("\n[cancelled]");
                history.pop();
                eprintln!();
            }
            Err(e) => {
                eprintln!("error: {e}");
                history.pop();
                eprintln!();
            }
        }

        let _ = stdout.flush();
    }

    signal_handle.abort();
    Ok(())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

fn tool_emoji(name: &str) -> &'static str {
    match name {
        "bash" | "terminal" => "⚡",
        "read_file" | "write_file" => "📄",
        "search_files" => "🔰",
        "memory" => "🧠",
        _ => "🔧",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
    }
}
