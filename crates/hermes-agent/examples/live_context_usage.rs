use std::path::PathBuf;
use std::time::Duration;

use perry_hermes_agent::{AgentLoop, AgentRunError, LoopEvent, PerryHermesConfig};
use perry_hermes_core::LoopError;
use perry_hermes_core::message::Content;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(default_config_path)
        .unwrap_or_else(|| {
            eprintln!(
                "usage: cargo run -p perry-hermes-agent --example live_context_usage -- <config>"
            );
            std::process::exit(2);
        });

    let config = match PerryHermesConfig::from_path(&config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: failed to load {}: {err}", config_path.display());
            std::process::exit(2);
        }
    };
    let selected_provider = match config.resolve_provider() {
        Ok(provider) => provider,
        Err(err) => {
            eprintln!("error: failed to resolve provider: {err}");
            std::process::exit(2);
        }
    };
    let provider_kind = format!("{:?}", selected_provider.kind);
    let provider_name = selected_provider.name.clone();
    let model = selected_provider.model.clone();
    let base_url = selected_provider
        .base_url
        .clone()
        .unwrap_or_else(|| "?".into());

    eprintln!(
        "provider={provider_name} kind={provider_kind} model={model} base_url={base_url} context_window={}",
        selected_provider.context_window_size
    );

    let agent = match AgentLoop::from_config(config) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("error: failed to build agent: {err}");
            std::process::exit(2);
        }
    };

    let session = agent.new_session(
        "live-context-usage",
        std::env::current_dir().unwrap_or_default(),
    );
    let cancel = CancellationToken::new();
    let prompts = [
        "Please remember this live context probe marker: perry-hermes-context-usage. Reply in one short sentence.",
        "Reply with exactly this marker and nothing else: perry-hermes-context-usage",
    ];

    for (idx, prompt) in prompts.iter().enumerate() {
        eprintln!("\nturn={} prompt_chars={}", idx + 1, prompt.len());

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(180),
            agent.run_session_turn(prompt, &session, cancel.clone(), move |event| match event {
                LoopEvent::Thinking => {}
                LoopEvent::ContextUsageUpdated { used_tokens } => {
                    eprintln!("\ncontext_usage[provider]={used_tokens}");
                }
                LoopEvent::ContentDelta(s) => eprint!("{s}"),
                LoopEvent::ReasoningDelta(_) => {}
                LoopEvent::ToolCallStarted { call, .. } => {
                    eprintln!("\ntool_started name={} args={}", call.name, call.arguments);
                }
                LoopEvent::ToolCallFinished { result, .. } => {
                    eprintln!("tool_finished ok={}", result.is_ok());
                }
                LoopEvent::AssistantMessage(_)
                | LoopEvent::ToolCallPartial(_)
                | LoopEvent::LengthLimit
                | LoopEvent::IterationsExhausted
                | LoopEvent::Cancelled
                | LoopEvent::CompressionCompleted { .. }
                | LoopEvent::CompressionSkipped { .. }
                | LoopEvent::CompressionFailed { .. } => {}
            }),
        )
        .await;

        let run_result = match result {
            Err(_) => {
                eprintln!("error: turn timed out after 180s");
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
            Ok(Err(err)) => {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
            Ok(Ok(result)) => result,
        };

        let answer = match &run_result.final_message.content {
            Content::Text(text) => text.as_str(),
            Content::Parts(_) => "<multimodal content>",
        };
        eprintln!(
            "\nturn_done iterations={} input={} cached_input={} output={} elapsed_ms={} answer={answer:?}",
            run_result.metrics.iterations,
            run_result.metrics.input_tokens,
            run_result.metrics.cached_input_tokens,
            run_result.metrics.output_tokens,
            started.elapsed().as_millis()
        );
    }
}

fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".perry_hermes")
            .join("config.toml"),
    )
}
