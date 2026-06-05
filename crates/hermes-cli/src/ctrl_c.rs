//! Ctrl+C decision logic for the REPL.
//!
//! Owns the "are we currently inside a turn?" state plus the active
//! `CancellationToken` for that turn. When the signal listener (spawned
//! separately in `run_repl`) receives Ctrl+C, it calls [`CtrlCHandler::handle`]
//! and acts on the returned [`CtrlCAction`].
//!
//! Behavior matches the spirit of the Python Hermes CLI: in-turn Ctrl+C
//! cancels the running turn; idle Ctrl+C exits. This shape is also
//! streaming-friendly for Phase 5 — the same `CancellationToken` already
//! flows through the provider's `select!` and will abort an in-flight
//! stream cleanly.

use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

/// What the REPL should do when Ctrl+C arrives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CtrlCAction {
    /// No turn is in flight — the REPL should exit.
    Exit,
    /// A turn is in flight — cancel its token and stay in the REPL.
    Cancel,
}

#[derive(Default)]
struct State {
    in_turn: bool,
    current_cancel: Option<CancellationToken>,
}

/// Thread-safe Ctrl+C decision maker for the REPL.
///
/// Cheap to clone via `Arc` if you need to share the same instance
/// between the REPL body and the signal listener task.
pub(crate) struct CtrlCHandler {
    state: Mutex<State>,
}

impl Default for CtrlCHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CtrlCHandler {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    /// Mark that a turn has started and register its cancellation token.
    /// The listener will call [`handle`](Self::handle) and route to
    /// [`CtrlCAction::Cancel`] if Ctrl+C arrives before [`exit_turn`](Self::exit_turn).
    pub(crate) fn enter_turn(&self, cancel: CancellationToken) {
        let mut state = self.state.lock().expect("ctrl_c mutex poisoned");
        state.in_turn = true;
        state.current_cancel = Some(cancel);
    }

    /// Mark that the current turn has finished. The next Ctrl+C
    /// will be routed to [`CtrlCAction::Exit`].
    pub(crate) fn exit_turn(&self) {
        let mut state = self.state.lock().expect("ctrl_c mutex poisoned");
        state.in_turn = false;
        state.current_cancel = None;
    }

    /// Decide what to do when Ctrl+C arrives. The caller is responsible
    /// for actually performing the action (e.g. `std::process::exit(0)`
    /// for [`CtrlCAction::Exit`], or letting the existing token do its
    /// job for [`CtrlCAction::Cancel`]).
    pub(crate) fn handle(&self) -> CtrlCAction {
        let state = self.state.lock().expect("ctrl_c mutex poisoned");
        if state.in_turn {
            if let Some(token) = state.current_cancel.as_ref() {
                token.cancel();
            }
            CtrlCAction::Cancel
        } else {
            CtrlCAction::Exit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_ctrl_c_signals_exit() {
        let handler = CtrlCHandler::new();
        assert_eq!(handler.handle(), CtrlCAction::Exit);
    }

    #[test]
    fn in_turn_ctrl_c_cancels_token() {
        let handler = CtrlCHandler::new();
        let cancel = CancellationToken::new();
        handler.enter_turn(cancel.clone());

        assert_eq!(handler.handle(), CtrlCAction::Cancel);
        assert!(cancel.is_cancelled(), "token should be cancelled");
    }

    #[test]
    fn exit_turn_resets_to_idle() {
        let handler = CtrlCHandler::new();
        let cancel = CancellationToken::new();
        handler.enter_turn(cancel.clone());
        handler.exit_turn();

        // Now idle — Ctrl+C should exit, and the previous token must NOT
        // be touched (a fresh turn with a fresh token will be installed).
        assert_eq!(handler.handle(), CtrlCAction::Exit);
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn fresh_turn_replaces_previous_cancel() {
        // Simulates: turn 1 starts → user Ctrl+C → turn 1 ends → turn 2
        // starts with a NEW token. The new token must not be pre-cancelled.
        let handler = CtrlCHandler::new();
        let cancel_1 = CancellationToken::new();
        handler.enter_turn(cancel_1.clone());
        let _ = handler.handle();
        assert!(cancel_1.is_cancelled());
        handler.exit_turn();

        let cancel_2 = CancellationToken::new();
        handler.enter_turn(cancel_2.clone());
        assert!(!cancel_2.is_cancelled(), "fresh turn must start with a fresh token");
        assert_eq!(handler.handle(), CtrlCAction::Cancel);
        assert!(cancel_2.is_cancelled());
    }

    #[test]
    fn handle_is_safe_to_call_concurrently() {
        use std::sync::Arc;
        use std::thread;

        let handler = Arc::new(CtrlCHandler::new());
        let cancel = CancellationToken::new();
        handler.enter_turn(cancel.clone());

        let mut joins = Vec::new();
        for _ in 0..8 {
            let h = Arc::clone(&handler);
            joins.push(thread::spawn(move || h.handle()));
        }
        for j in joins {
            assert_eq!(j.join().unwrap(), CtrlCAction::Cancel);
        }
        assert!(cancel.is_cancelled());
    }
}
