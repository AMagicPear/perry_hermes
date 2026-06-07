//! Token-counting + argument-validation helpers used by the agent loop.
//!
//! Token counts in this crate come exclusively from `Usage` after a
//! provider responds (see `perry_hermes_core::Usage::prompt_context_tokens`).
//! The agent loop never estimates — the TUI shows `0` until the first
//! real response arrives.

use perry_hermes_core::Usage;

/// `input_tokens + cached_input_tokens` — the value used for the
/// `LoopEvent::ContextUsageUpdated` event after a real provider response.
pub fn prompt_context_tokens_from_usage(usage: Usage) -> u64 {
    usage.prompt_context_tokens()
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
