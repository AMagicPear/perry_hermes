//! Token-counting helpers used by the agent loop and the compressor.
//!
//! All estimates use the same conservative `4 chars ≈ 1 token` heuristic
//! the rest of the codebase uses (`hermes_core::CHARS_PER_TOKEN_ESTIMATE`).
//! No real tokenization is performed.

use hermes_core::message::Message;
use hermes_core::registry::ToolSchema;
use hermes_core::Usage;

/// Estimate total tokens for a list of messages.
pub fn estimate_tokens_for_messages(messages: &[Message], chars_per_token: f64) -> u64 {
    let total_chars: usize = messages.iter().map(Message::char_len).sum();
    (total_chars as f64 / chars_per_token) as u64
}

/// `input_tokens + cached_input_tokens` — the value used for the
/// `LoopEvent::ContextUsageUpdated` event after a real provider response.
pub fn prompt_context_tokens_from_usage(usage: Usage) -> u64 {
    usage.prompt_context_tokens()
}

/// Estimate the total context-usage (messages + tool schemas) in tokens
/// before sending a request. Used for the pre-flight
/// `LoopEvent::ContextUsageUpdated` event so the TUI can show a running
/// total before the real provider usage arrives.
pub fn estimate_request_context_tokens(
    messages: &[Message],
    tools: &[ToolSchema],
    chars_per_token: f64,
) -> u64 {
    let message_chars: usize = messages.iter().map(Message::char_len).sum();
    let tool_chars: usize = tools
        .iter()
        .filter_map(|tool| serde_json::to_string(tool).ok())
        .map(|s| s.len())
        .sum();
    ((message_chars + tool_chars) as f64 / chars_per_token) as u64
}

/// Validate tool-call arguments against a JSON Schema. Returns the args
/// unchanged on success; the error string joins every validation failure
/// (semicolon-separated) for the LLM to read.
pub fn validate_args(
    args: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use jsonschema::{Draft, JSONSchema};
    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(schema)
        .map_err(|e| format!("schema compile: {e}"))?;
    let result = compiled.validate(args);
    if let Err(errors) = result {
        let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(msgs.join("; "));
    }
    Ok(args.clone())
}
