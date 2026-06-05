//! Hermes CLI — interactive REPL for the Hermes agent.
//!
//! Phase 4: reads user input line by line, sends it to the agent loop,
//! and renders tool call events to stderr. Supports `--provider echo`
//! for offline smoke testing.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

use hermes_core::error::LoopError;
use hermes_core::message::{Content, Message, Role};
use hermes_runtime::{AIAgent, AgentOptions, LoopEvent, DEFAULT_SYSTEM_PROMPT};
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
    /// Provider: "openai", "anthropic", or "echo"
    #[arg(long, default_value = "openai")]
    provider: String,

    /// Model name (default: provider-specific env var or built-in default)
    #[arg(long)]
    model: Option<String>,

    /// API base URL (default: provider-specific env var or built-in default)
    #[arg(long)]
    base_url: Option<String>,

    /// Max iterations per turn (default: 10, matching Python Hermes CLI)
    #[arg(long, default_value_t = 10)]
    max_iterations: u32,

    /// Disabled toolsets (e.g. "terminal" to disable bash, "core" for all)
    #[arg(long, value_delimiter = ',')]
    disabled_toolsets: Vec<String>,

    /// Working directory for tools
    #[arg(long)]
    cwd: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let tokio_rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    tokio_rt.block_on(async { dispatch(args).await })
}

async fn dispatch(args: Args) -> anyhow::Result<()> {
    let working_dir = args
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let options = AgentOptions {
        max_iterations: args.max_iterations,
        system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into()),
        disabled_toolsets: args.disabled_toolsets.clone(),
        working_dir: working_dir.clone(),
        session_id: "cli".into(),
    };

    match args.provider.as_str() {
        "echo" => {
            let agent = AIAgent::echo(options);
            run_repl(agent).await
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is not set. Export it or use direnv.")?;
            let model = args
                .model
                .clone()
                .or_else(|| std::env::var("OPENAI_MODEL").ok())
                .unwrap_or_else(|| "gpt-4o-mini".into());
            let base_url = args
                .base_url
                .clone()
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com/v1".into());
            let agent = AIAgent::openai_compatible(api_key, model, base_url, options);
            run_repl(agent).await
        }
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY is not set. Export it or use direnv.")?;
            let model = args
                .model
                .clone()
                .or_else(|| std::env::var("ANTHROPIC_MODEL").ok())
                .unwrap_or_else(|| "claude-sonnet-4-5".into());
            let base_url = args
                .base_url
                .clone()
                .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.anthropic.com/v1".into());
            let api_key_header =
                std::env::var("ANTHROPIC_API_KEY_HEADER").unwrap_or_else(|_| "x-api-key".into());
            let agent = AIAgent::anthropic_with_api_key_header(
                api_key,
                model,
                base_url,
                api_key_header,
                options,
            );
            run_repl(agent).await
        }
        other => bail!("unknown provider: {other}. Use 'openai', 'anthropic', or 'echo'."),
    }
}

async fn run_repl(agent: AIAgent) -> anyhow::Result<()> {
    eprintln!(
        "hermes v{} — type a message, Ctrl-D to quit, Ctrl-C to cancel a turn or (when idle) quit",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut history: Vec<Message> = Vec::new();

    // Persistent Ctrl+C listener for the entire REPL. Behavior matches the
    // Python Hermes CLI: in-turn Ctrl+C cancels the current turn via
    // CancellationToken; idle Ctrl+C exits the process. This shape is
    // forward-compatible with Phase 5 streaming — the same cancel token
    // already aborts in-flight HTTP reads in the provider layer.
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
                    CtrlCAction::Cancel => {
                        // Stay in the REPL; the in-flight turn sees the
                        // cancelled token and returns. The listener keeps
                        // running so the NEXT Ctrl+C (now back at the
                        // prompt) can exit.
                    }
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
            .run_messages(history.clone(), cancel.clone(), |event| match event {
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
                LoopEvent::ToolCallPartial(_) => {
                    // Silent — ToolCallStarted fires when complete.
                }
            })
            .await;

        ctrl_c.exit_turn();

        match result {
            Ok(run_result) => {
                // Don't re-print the final message — it was already streamed
                // token-by-token via the ContentDelta on_event arm. Just
                // print the metrics. The AssistantMessage event also fires
                // a newline (via its on_event arm) so the cursor is on a
                // fresh line before the metrics block.
                let _ = &run_result.final_message; // suppress unused warning

                // Update history with full trajectory for multi-turn context.
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

    // The /exit /quit / Ctrl-D paths all reach here; abort the persistent
    // signal task so it doesn't outlive the REPL. (Ctrl+C idle-exit calls
    // std::process::exit and skips this — that's fine, the OS reaps it.)
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
        "search_files" => "🔍",
        "memory" => "🧠",
        _ => "🔧",
    }
}
