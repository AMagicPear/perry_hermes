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
    let validator = jsonschema::draft7::options()
        .build(schema)
        .map_err(|e| format!("schema compile: {e}"))?;
    if let Err(e) = validator.validate(args) {
        return Err(e.to_string());
    }
    Ok(args.clone())
}
