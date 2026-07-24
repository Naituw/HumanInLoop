//! First-terminal coordinator for structured confirmations.
//!
//! Unlike ordinary Ask, a human decision is emitted immediately after it wins the atomic gate;
//! popup/IM finalization is owned by daemon-side channel tasks and never delays the caller.

use crate::models::{ConfirmFallbackReason, ConfirmRequest, ConfirmResult};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

use super::terminal_gate::FirstTerminalGate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Final(ConfirmResult),
    Fallback(ConfirmFallbackReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Terminal {
    Final(ConfirmResult),
    Fallback(ConfirmFallbackReason),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmTerminalKind {
    Decision(ConfirmResult),
    Fallback(ConfirmFallbackReason),
    Cancelled,
}

/// Two-phase commit hook run for the winning submission only, before the terminal gate is
/// set and any surface can render a final state (spec codex-permission-remember §5.6). It may
/// rewrite the result, e.g. degrade a remember choice to `approve_once` when persisting the
/// rules failed (D25).
pub type ConfirmFinalizer = Arc<dyn Fn(ConfirmResult) -> ConfirmResult + Send + Sync>;

pub struct ConfirmCoordinator {
    request: Arc<ConfirmRequest>,
    terminal: FirstTerminalGate<Terminal>,
    tx: UnboundedSender<ConfirmOutcome>,
    finalizer: Option<ConfirmFinalizer>,
    /// Serializes submissions so the finalizer runs at most once, for the winner only.
    submit_lock: Mutex<()>,
}

impl ConfirmCoordinator {
    pub fn new(request: Arc<ConfirmRequest>, tx: UnboundedSender<ConfirmOutcome>) -> Arc<Self> {
        Self::with_finalizer(request, tx, None)
    }

    pub fn with_finalizer(
        request: Arc<ConfirmRequest>,
        tx: UnboundedSender<ConfirmOutcome>,
        finalizer: Option<ConfirmFinalizer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            request,
            terminal: FirstTerminalGate::new(),
            tx,
            finalizer,
            submit_lock: Mutex::new(()),
        })
    }

    pub fn request(&self) -> &ConfirmRequest {
        &self.request
    }

    /// Resolve a wire choice and emit it only if this call wins the terminal gate.
    /// Validation errors leave the request pending so the same or another surface may retry.
    pub fn submit_wire(
        &self,
        choice_index: usize,
        comment: Option<String>,
        source_channel_id: impl Into<String>,
    ) -> Result<bool, String> {
        let result = self
            .request
            .resolve_submission(choice_index, comment, source_channel_id)?;
        let _guard = self.submit_lock.lock().unwrap();
        if self.terminal.is_set() {
            return Ok(false);
        }
        let result = match &self.finalizer {
            Some(finalizer) => finalizer(result),
            None => result,
        };
        if !self.terminal.try_set(Terminal::Final(result.clone())) {
            return Ok(false);
        }
        let _ = self.tx.send(ConfirmOutcome::Final(result));
        Ok(true)
    }

    pub fn fallback(&self, reason: ConfirmFallbackReason) -> bool {
        if !self.terminal.try_set(Terminal::Fallback(reason)) {
            return false;
        }
        let _ = self.tx.send(ConfirmOutcome::Fallback(reason));
        true
    }

    /// Caller disconnected or daemon is being forcibly stopped. There is no receiver left, so
    /// this closes the gate without emitting a synthetic human decision or fallback frame.
    pub fn cancel(&self) -> bool {
        self.terminal.try_set(Terminal::Cancelled)
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal.is_set()
    }

    pub fn winner_channel_id(&self) -> Option<String> {
        self.terminal.with(|terminal| match terminal {
            Some(Terminal::Final(result)) => Some(result.source_channel_id.clone()),
            _ => None,
        })
    }

    pub fn terminal_kind(&self) -> Option<ConfirmTerminalKind> {
        self.terminal.with(|terminal| match terminal {
            Some(Terminal::Final(result)) => Some(ConfirmTerminalKind::Decision(result.clone())),
            Some(Terminal::Fallback(reason)) => Some(ConfirmTerminalKind::Fallback(*reason)),
            Some(Terminal::Cancelled) => Some(ConfirmTerminalKind::Cancelled),
            None => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::ActionRole;
    use crate::models::{ConfirmChoice, ConfirmDetail, ConfirmPresentation, ConfirmSpec};

    fn coordinator() -> (
        Arc<ConfirmCoordinator>,
        tokio::sync::mpsc::UnboundedReceiver<ConfirmOutcome>,
    ) {
        let spec = ConfirmSpec {
            title: "Approve?".into(),
            context: vec![],
            detail: ConfirmDetail {
                summary: "Run command".into(),
                body_md: String::new(),
            },
            choices: vec![
                ConfirmChoice {
                    id: "approve_once".into(),
                    label: "Approve once".into(),
                    description: String::new(),
                    role: ActionRole::Primary,
                    variant: None,
                },
                ConfirmChoice {
                    id: "deny".into(),
                    label: "Deny".into(),
                    description: String::new(),
                    role: ActionRole::Destructive,
                    variant: None,
                },
            ],
            presentation: ConfirmPresentation::SingleSelectSubmit {
                input: None,
                submit_label: "Submit".into(),
                default_action_id: None,
            },
            dismiss_action_id: "deny".into(),
        };
        let request = Arc::new(spec.into_request("r1".into(), 1, 2).unwrap());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (ConfirmCoordinator::new(request, tx), rx)
    }

    #[tokio::test]
    async fn first_valid_result_wins_and_is_emitted_immediately() {
        let (coordinator, mut rx) = coordinator();
        assert!(coordinator
            .submit_wire(0, None, "popup")
            .expect("valid submission"));
        assert!(!coordinator
            .submit_wire(1, None, "feishu")
            .expect("late valid submission"));
        match rx.recv().await.unwrap() {
            ConfirmOutcome::Final(result) => {
                assert_eq!(result.action_id, "approve_once");
                assert_eq!(result.source_channel_id, "popup");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn invalid_wire_choice_does_not_close_gate() {
        let (coordinator, mut rx) = coordinator();
        assert!(coordinator.submit_wire(9, None, "popup").is_err());
        assert!(!coordinator.is_terminal());
        assert!(coordinator.fallback(ConfirmFallbackReason::Expired));
        assert_eq!(
            rx.recv().await,
            Some(ConfirmOutcome::Fallback(ConfirmFallbackReason::Expired))
        );
    }

    #[test]
    fn cancellation_never_becomes_a_human_decision() {
        let (coordinator, mut rx) = coordinator();
        assert!(coordinator.cancel());
        assert!(!coordinator.fallback(ConfirmFallbackReason::Expired));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn finalizer_runs_once_for_winner_and_may_rewrite_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let spec = ConfirmSpec {
            title: "Approve?".into(),
            context: vec![],
            detail: ConfirmDetail {
                summary: "Run command".into(),
                body_md: String::new(),
            },
            choices: vec![
                ConfirmChoice {
                    id: "remember_files".into(),
                    label: "Remember".into(),
                    description: String::new(),
                    role: ActionRole::Default,
                    variant: None,
                },
                ConfirmChoice {
                    id: "deny".into(),
                    label: "Deny".into(),
                    description: String::new(),
                    role: ActionRole::Destructive,
                    variant: None,
                },
            ],
            presentation: ConfirmPresentation::SingleSelectSubmit {
                input: None,
                submit_label: "Submit".into(),
                default_action_id: None,
            },
            dismiss_action_id: "deny".into(),
        };
        let request = Arc::new(spec.into_request("r1".into(), 1, 2).unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_finalizer = calls.clone();
        let coordinator = ConfirmCoordinator::with_finalizer(
            request,
            tx,
            Some(Arc::new(move |mut result: ConfirmResult| {
                calls_in_finalizer.fetch_add(1, Ordering::SeqCst);
                // Simulate a failed save degrading to allow-once (D25).
                result.action_id = "approve_once".into();
                result
            })),
        );
        assert!(coordinator.submit_wire(0, None, "popup").unwrap());
        // The loser never triggers the finalizer again.
        assert!(!coordinator.submit_wire(0, None, "feishu").unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        match rx.recv().await.unwrap() {
            ConfirmOutcome::Final(result) => assert_eq!(result.action_id, "approve_once"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
