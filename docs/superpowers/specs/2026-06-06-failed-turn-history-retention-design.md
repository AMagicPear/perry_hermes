# Failed Turn History Retention

**Date:** 2026-06-06
**Status:** Proposed
**Supersedes:** current REPL behavior that drops the active user turn from history whenever `run_messages(...)` returns a non-cancel error

> **Note (Phase 10):** the "REPL" referenced here is the legacy `hermes-cli` REPL that was the product shell at the time of this spec. Phase 10 replaces it with a `ratatui` TUI; the failure-handling semantics this spec defines still apply, but the `history` state is now held inside the TUI's session model rather than the REPL's loop.

## 1. Goal

Preserve semantically meaningful output from a failed turn so the next user message continues with the same conversation state the user just saw in the terminal.

After this change, a turn that fails after partial assistant output or after one or more completed tool calls should still contribute those messages to the next provider request. This includes:

- the current user message
- any assistant text already streamed or finalized
- any completed tool result messages
- a plain assistant error message explaining why the turn stopped

This does **not** include CLI-only rendering artifacts such as emojis, iteration counters, or bracketed status summaries.

## 2. Non-Goals

- Persisting pure CLI status lines like `[iterations=...]`, `📦`, or `←`
- Changing successful turn behavior
- Retaining empty failed turns that produced no assistant/tool content
- Altering Ctrl-C cancellation semantics beyond what is needed to keep existing partial-message behavior

## 3. Current Problem

Today the REPL pushes the new user message into `history` before calling `run_messages(...)`. If the run succeeds, `history` is replaced with `run_result.messages`. If the run returns a generic error, the REPL prints the error and then executes `history.pop()`.

That means a provider failure after completed tool output loses the entire in-flight turn from future context, even though the user already saw meaningful output in the terminal.

## 4. Desired Behavior

### 4.1 Successful turns

No change. `history` continues to be replaced with the full `run_result.messages`.

### 4.2 Failed turns with partial conversation state

When a non-cancel error occurs after the loop has already accumulated meaningful messages for the current turn, the next turn should inherit those messages.

The retained history should contain:

1. the current user message
2. any assistant message that was already finalized into the loop's message list
3. any tool messages already appended by completed tool calls
4. one synthetic assistant message containing the failure explanation

Recommended wording for the synthetic assistant message:

`Turn interrupted by error: provider error: ...`

The message should be plain text and should not include CLI decoration.

### 4.3 Failed turns with no meaningful output

If the failure happens before any assistant or tool content exists for this turn, the REPL may keep the current fallback behavior and discard the just-entered user message. This spec only requires retaining content that the user already saw as semantic turn output.

## 5. Implementation Shape

Introduce a loop error variant that carries partial history for non-cancel failures:

```rust
LoopError::FailedWithPartial {
    messages: Vec<Message>,
    source: ProviderError,
}
```

The loop should return this variant when a provider failure happens after it has already built up a meaningful `messages` list for the active turn. The messages payload should reuse the same internal conversation vector the loop was already maintaining.

The REPL should handle this variant by:

1. replacing `history` with the returned partial messages
2. appending one assistant text message with the formatted error string
3. printing the error as it does today

If the loop returns a plain fatal error with no partial state, the REPL may keep the current fallback behavior.

## 6. Testing

Add regression coverage for the case:

1. user sends a prompt
2. assistant emits a tool call
3. tool execution succeeds and appends a tool message
4. the follow-up provider call fails
5. the next turn still includes the prior user message, the assistant tool-call message, the tool result, and the synthesized assistant error message

Also cover an early provider failure before any assistant/tool output to ensure the old fallback path still drops the empty turn.
