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

use hermes_core::message::{Content, Message, Role};
use hermes_core::provider::Provider;
use hermes_core::registry::{InMemoryRegistry, ToolRegistry};
use hermes_core::tool::ToolContext;
use hermes_loop::{AgentLoop, LoopConfig, LoopEvent};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "hermes",
    version,
    about = "Hermes — AI agent with tool use",
    long_about = None
)]
struct Args {
    /// Provider: "openai" or "echo"
    #[arg(long, default_value = "openai")]
    provider: String,

    /// Model name (default: env OPENAI_MODEL or "gpt-4o-mini")
    #[arg(long)]
    model: Option<String>,

    /// API base URL (default: env OPENAI_BASE_URL)
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
    let disabled = args.disabled_toolsets.clone();
    let tokio_rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    tokio_rt.block_on(async { dispatch(args, disabled).await })
}

/// Build provider + registry, then branch on provider type to enter the REPL.
/// Each branch instantiates `AgentLoop` with a concrete provider type, avoiding
/// the need for `Arc<dyn Provider>` (which doesn't satisfy `P: Provider`).
async fn dispatch(args: Args, disabled_toolsets: Vec<String>) -> anyhow::Result<()> {
    let config = loop_config(&args);
    let working_dir = args
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    match args.provider.as_str() {
        "echo" => {
            let provider = hermes_providers::EchoProvider::new();
            let registry = build_registry(&disabled_toolsets);
            let loop_ = AgentLoop::new(provider, Arc::new(registry), config);
            run_repl(loop_, working_dir).await
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
            let provider =
                hermes_providers::OpenAiProvider::new(api_key, model).with_base_url(base_url);
            let registry = build_registry(&disabled_toolsets);
            let loop_ = AgentLoop::new(provider, Arc::new(registry), config);
            run_repl(loop_, working_dir).await
        }
        other => bail!("unknown provider: {other}. Use 'openai' or 'echo'."),
    }
}

/// Generic REPL loop — works with any `AgentLoop<P, R>`.
async fn run_repl<P: Provider, R: ToolRegistry>(
    loop_: AgentLoop<P, R>,
    working_dir: PathBuf,
) -> anyhow::Result<()> {
    eprintln!(
        "hermes v{} — type a message, Ctrl-D or Ctrl-C to quit",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut history: Vec<Message> = Vec::new();

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

        let ctx = ToolContext {
            session_id: "cli".into(),
            working_dir: working_dir.clone(),
            permissions: Default::default(),
        };
        let cancel = CancellationToken::new();

        // Ctrl-C: first cancels the current turn, second exits REPL.
        let cancel_clone = cancel.clone();
        let ctrl_c_handle = tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_clone.cancel();
                    return;
                }
            }
        });

        let result = loop_
            .run(history.clone(), ctx, cancel.clone(), |event| match event {
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
            })
            .await;

        ctrl_c_handle.abort();

        match result {
            Ok(run_result) => {
                let text = match run_result.final_message.content {
                    Content::Text(s) => s,
                    Content::Parts(_) => "<multimodal content>".into(),
                };
                println!("{text}");

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
            Err(e) => {
                eprintln!("error: {e}");
                history.pop(); // remove the unprocessed user message
                eprintln!();
            }
        }

        let _ = stdout.flush();
    }

    Ok(())
}

fn build_registry(disabled_toolsets: &[String]) -> InMemoryRegistry {
    let mut registry = InMemoryRegistry::new();
    if !disabled_toolsets.contains(&"core".to_string())
        && !disabled_toolsets.contains(&"terminal".to_string())
    {
        registry = registry.register(Arc::new(BashTool::new()));
    }
    registry
}

fn loop_config(args: &Args) -> LoopConfig {
    LoopConfig {
        max_iterations: args.max_iterations,
        system_prompt: Some(
            "You are a careful assistant with access to a `bash` tool. \
             Use it to inspect the system or run shell commands when \
             needed. When you have enough information to answer, give \
             a concise final response — do not call tools again."
                .into(),
        ),
        ..Default::default()
    }
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
