use std::time::Duration;

use hermes_agent::{AIAgent, AgentRunError, AgentSession, HermesConfig, LoopEvent};
use hermes_core::message::Content;
use hermes_core::LoopError;
use hermes_providers::OpenAiProvider;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("error: OPENAI_API_KEY is not set");
        std::process::exit(2);
    };
    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let user_text: String = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "what's my kernel version? use bash.".into());

    let provider = OpenAiProvider::new(&api_key, &model).with_base_url(&base_url);
    let agent = AIAgent::new(provider, HermesConfig::default());
    let session = AgentSession::current_shell();
    let cancel = CancellationToken::new();

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(180),
        agent.run_session_turn(&user_text, &session, cancel, |event| match event {
            LoopEvent::Thinking => {}
            LoopEvent::AssistantMessage(_) => {}
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
            LoopEvent::ContextUsageUpdated { .. } => {}
            LoopEvent::CompressionCompleted { .. } => {}
            LoopEvent::CompressionSkipped { .. } => {}
            LoopEvent::CompressionFailed { .. } => {}
        }),
    )
    .await;

    match result {
        Err(_) => {
            eprintln!("error: request timed out after 180s");
            std::process::exit(1);
        }
        Ok(Err(AgentRunError::Loop(LoopError::MaxIterations(n)))) => {
            eprintln!("error: max iterations ({n}) reached");
            std::process::exit(1);
        }
        Ok(Err(AgentRunError::FailedTurn { source, .. })) => {
            eprintln!("error: provider error: {source}");
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
