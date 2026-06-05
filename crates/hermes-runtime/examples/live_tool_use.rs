//! Live end-to-end smoke for Phase 3: OpenAI/MiniMax + AgentLoop + BashTool.
//!
//! Sends a question that requires using bash to the model, watches the
//! LLM call the tool, and prints the final answer. Same env-var
//! contract as `live_smoke` in `hermes-providers/examples/`:
//!
//! ```bash
//! # with direnv autoloading .envrc:
//! cargo run -p hermes-runtime --example live_tool_use -- "what's my kernel version?"
//!
//! # or inline:
//! OPENAI_API_KEY=sk-... OPENAI_BASE_URL=https://api.minimaxi.com/v1 \
//!   OPENAI_MODEL=MiniMax-M3 \
//!   cargo run -p hermes-runtime --example live_tool_use -- "what's my kernel version?"
//! ```

use std::time::Duration;

use hermes_core::message::Content;
use hermes_core::LoopError;
use hermes_runtime::{AIAgent, AgentOptions, LoopEvent};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            eprintln!("error: OPENAI_API_KEY is not set");
            eprintln!();
            eprintln!("either export it, or use direnv to auto-load a project-local .envrc.");
            std::process::exit(2);
        }
    };
    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let user_text: String = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "what's my kernel version? use bash.".into());

    eprintln!("→ POST {base_url}/chat/completions (model={model})");
    eprintln!("→ user: {user_text}");
    eprintln!();

    let agent = AIAgent::openai_compatible(&api_key, &model, &base_url, AgentOptions::default());
    let cancel = CancellationToken::new();

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(180),
        agent.run_turn(&user_text, cancel, |event| match event {
            LoopEvent::Thinking => {
                eprint!("… thinking");
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
            LoopEvent::AssistantMessage(_) => {
                eprintln!();
            }
            LoopEvent::ToolCallStarted { call, .. } => {
                eprintln!("→ tool: {}({})", call.name, call.arguments);
            }
            LoopEvent::ToolCallFinished { result, .. } => match result {
                Ok(out) => {
                    let preview: String = out.content.chars().take(160).collect();
                    eprintln!(
                        "← {} {}",
                        preview,
                        if out.content.len() > 160 { "…" } else { "" }
                    );
                }
                Err(e) => eprintln!("← error: {e}"),
            },
            LoopEvent::LengthLimit => eprintln!("[hit length limit]"),
            LoopEvent::IterationsExhausted => eprintln!("[max iterations]"),
            LoopEvent::Cancelled => eprintln!("[cancelled]"),
            LoopEvent::ContentDelta(s) => eprint!("{s}"),
            LoopEvent::ReasoningDelta(s) => eprint!("{s}"),
            LoopEvent::ToolCallPartial(_) => {}
        }),
    )
    .await;

    match result {
        Err(_) => {
            eprintln!("error: request timed out after 180s");
            std::process::exit(1);
        }
        Ok(Err(LoopError::MaxIterations(n))) => {
            eprintln!("error: max iterations ({n}) reached");
            std::process::exit(1);
        }
        Ok(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Ok(Ok(r)) => {
            let text = match r.final_message.content {
                Content::Text(s) => s,
                Content::Parts(_) => "<multimodal content>".into(),
            };
            println!("{text}");
            eprintln!();
            eprintln!(
                "← iterations={} tool_calls={} in={} out={} elapsed={:?}",
                r.metrics.iterations,
                r.metrics.tool_calls,
                r.metrics.input_tokens,
                r.metrics.output_tokens,
                started.elapsed()
            );
        }
    }
}
