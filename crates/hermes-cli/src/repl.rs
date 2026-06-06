use std::io::{self, BufRead, Write};
use std::sync::Arc;

use anyhow::Context;
use hermes_agent::{AIAgent, AgentRunError, LoopEvent, SessionContext};
use hermes_core::error::LoopError;
use hermes_core::message::{Content, Message, Role};
use tokio_util::sync::CancellationToken;

use crate::cli_render::{tool_emoji, truncate_str};
use crate::ctrl_c::{CtrlCAction, CtrlCHandler};

pub(crate) async fn run_repl(agent: AIAgent, session: &SessionContext) -> anyhow::Result<()> {
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
        if let Some(rest) = line.strip_prefix("/compact") {
            let focus = rest.trim();
            let focus = if focus.is_empty() { None } else { Some(focus) };
            match agent.run_compact(history.clone(), focus, session).await {
                Ok((new_history, event)) => {
                    history = new_history;
                    match event {
                        LoopEvent::CompressionCompleted {
                            trigger,
                            tokens_before,
                            tokens_after,
                            summary_chars,
                            duration,
                        } => {
                            eprintln!(
                                "  🗜️  {trigger:?}: {tokens_before} → {tokens_after} tokens (summary {summary_chars} chars, {:.1}s)",
                                duration.as_secs_f64()
                            );
                        }
                        LoopEvent::CompressionSkipped { reason } => {
                            eprintln!("  🗜️  skipped: {reason:?}");
                        }
                        LoopEvent::CompressionFailed { error, .. } => {
                            eprintln!("  🗜️  failed: {error}");
                        }
                        _ => eprintln!("  [context compacted]"),
                    }
                    eprintln!();
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    eprintln!();
                }
            }
            continue;
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
            .run_messages(
                history.clone(),
                session,
                cancel.clone(),
                |event| match event {
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
                    LoopEvent::CompressionCompleted {
                        trigger,
                        tokens_before,
                        tokens_after,
                        summary_chars,
                        duration,
                    } => {
                        eprintln!(
                            "
  🗜️  {trigger:?}: {tokens_before} → {tokens_after} tokens (summary {summary_chars} chars, {:.1}s)",
                            duration.as_secs_f64()
                        );
                    }
                    LoopEvent::CompressionSkipped { reason } => {
                        eprintln!(
                            "
  🗜️  skipped: {reason:?}"
                        );
                    }
                    LoopEvent::CompressionFailed { error, .. } => {
                        eprintln!(
                            "
  🗜️  failed: {error}"
                        );
                    }
                },
            )
            .await;

        ctrl_c.exit_turn();

        match result {
            Ok(run_result) => {
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
            Err(AgentRunError::Loop(LoopError::CancelledWith(partial))) => {
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
            Err(AgentRunError::Loop(LoopError::Cancelled)) => {
                eprintln!("\n[cancelled]");
                history.pop();
                eprintln!();
            }
            Err(AgentRunError::FailedTurn {
                failed_turn,
                source,
            }) => {
                eprintln!("error: provider error: {source}");
                history = failed_turn.messages;
                eprintln!();
            }
            Err(AgentRunError::Loop(e)) => {
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
