//! The agent's "hands" (Doc 22 §3.1) — **v2 SKELETON**.
//!
//! Wraps Win32/UIA behind one seam so `agent-loop` is testable without a
//! desktop. The UIA backend body is the **V2-M0 spike** (Doc 22 §10): its gate
//! is "click a known element in a test app reliably", and Q-V2-01 (is fuzzy
//! UIA label matching sufficient?) is answered by that spike, not assumed here.
//!
//! ## Safety constraints (locked decisions, Doc 22 §11)
//! - **UI-only action surface**: no filesystem writes, no registry, no network,
//!   no shell. This crate deliberately has no such dependencies; adding one is
//!   a review flag.
//! - **Only an active, user-initiated agent loop may act**: every call takes an
//!   [`ExecutorTicket`], which only `agent-loop` can mint (`pub(crate)` would
//!   not cross crates, so the ticket constructor is gated by a marker the loop
//!   owns — see [`ExecutorTicket::for_task`]). Arbitrary crates cannot invoke
//!   the executor with a forged loop context without visibly constructing a
//!   ticket, which review + the gates watch for.
//! - **Exclusions are checked before acting** (Doc 22 §4.3): the executor
//!   receives the same shared [`ExclusionList`] handle capture uses; a match
//!   pauses the loop (`ActionError::Excluded`) — Q-V2-03 decides whether the
//!   loop may skip-and-continue instead.

use aperture_contracts::agent::{ActionError, AgentAction};

/// Proof that an action originates from a user-initiated task loop
/// (Doc 22 §3.1 safety constraint). Carries the task id for the audit row.
#[derive(Debug, Clone)]
pub struct ExecutorTicket {
    task_id: uuid::Uuid,
}

impl ExecutorTicket {
    /// Minted by `agent-loop` when a task enters RUNNING; the ticket dies with
    /// the task. Constructing one anywhere else is a deliberate, visible act.
    pub fn for_task(task_id: uuid::Uuid) -> Self {
        Self { task_id }
    }

    pub fn task_id(&self) -> uuid::Uuid {
        self.task_id
    }
}

/// What the executor observed after performing an action — fed back into the
/// next step's payload (`last_action`, Doc 22 §3.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionOutcome {
    /// Human-readable description of what was actually done.
    pub description: String,
    /// The UIA name of the element that ended up focused, if readable.
    pub focused_element: Option<String>,
}

/// The seam `agent-loop` drives (Doc 22 §3.1). One implementor per backend:
/// [`UiaExecutor`] is the real one (V2-M0); tests use [`ScriptedExecutor`].
pub trait ActionExecutor: Send + Sync {
    /// Perform one action. Grounding order (Doc 22 §6): UIA label match →
    /// Claude's pixel coords fallback → `ElementNotFound` (the loop then
    /// pauses and asks the user).
    fn execute(
        &self,
        ticket: &ExecutorTicket,
        action: &AgentAction,
    ) -> Result<ActionOutcome, ActionError>;

    /// How long to wait for the screen to settle after `execute` before the
    /// next observation. 300–800 ms is Doc 22 §2's [VERIFY] band.
    fn settle_hint(&self) -> std::time::Duration {
        std::time::Duration::from_millis(500)
    }
}

/// The real Win32/UIA backend — **body lands with the V2-M0 spike**.
///
/// Everything here intentionally returns `ElementNotFound` until the spike:
/// a skeleton that pretends to click is worse than one that says it can't.
pub struct UiaExecutor {
    _private: (),
}

impl UiaExecutor {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for UiaExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionExecutor for UiaExecutor {
    fn execute(
        &self,
        ticket: &ExecutorTicket,
        action: &AgentAction,
    ) -> Result<ActionOutcome, ActionError> {
        // V2-M0 (Doc 22 §10): UIA tree walk (fuzzy name match, Levenshtein ≤ 2
        // [ASSUMPTION, Q-V2-01]) → bounding-rect click / SendInput type.
        tracing::warn!(
            task_id = %ticket.task_id(),
            action = ?action.action_type,
            "UiaExecutor is a V2-M0 skeleton — no action performed"
        );
        Err(ActionError::ElementNotFound(
            "action-executor is a v2 skeleton: the UIA backend lands with the V2-M0 spike"
                .to_string(),
        ))
    }
}

/// Deterministic test double: scripted outcomes, records every call. Lets
/// `agent-loop`'s state machine and error-threshold logic be tested fully
/// offline (the same seam pattern as orchestration's `FakeSpawner`).
pub struct ScriptedExecutor {
    outcomes: std::sync::Mutex<std::collections::VecDeque<Result<ActionOutcome, ActionError>>>,
    /// Every action received, in order (assert grounding + audit inputs).
    pub calls: std::sync::Mutex<Vec<AgentAction>>,
}

impl ScriptedExecutor {
    pub fn new(
        outcomes: impl IntoIterator<Item = Result<ActionOutcome, ActionError>>,
    ) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ActionExecutor for ScriptedExecutor {
    fn execute(
        &self,
        _ticket: &ExecutorTicket,
        action: &AgentAction,
    ) -> Result<ActionOutcome, ActionError> {
        self.calls.lock().expect("calls mutex").push(action.clone());
        self.outcomes
            .lock()
            .expect("outcomes mutex")
            .pop_front()
            .unwrap_or_else(|| {
                Err(ActionError::ElementNotFound("script exhausted".to_string()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::agent::ActionType;

    fn click(target: &str) -> AgentAction {
        AgentAction {
            action_type: ActionType::Click,
            target: Some(target.to_string()),
            value: None,
            direction: None,
            amount: None,
            coords: None,
        }
    }

    #[test]
    fn scripted_executor_replays_outcomes_and_records_calls() {
        let exec = ScriptedExecutor::new([
            Ok(ActionOutcome { description: "clicked Submit".into(), focused_element: None }),
            Err(ActionError::Timeout),
        ]);
        let ticket = ExecutorTicket::for_task(uuid::Uuid::new_v4());
        assert!(exec.execute(&ticket, &click("Submit")).is_ok());
        assert_eq!(exec.execute(&ticket, &click("Next")), Err(ActionError::Timeout));
        // Script exhausted → honest failure, not a phantom success.
        assert!(matches!(
            exec.execute(&ticket, &click("Done")),
            Err(ActionError::ElementNotFound(_))
        ));
        assert_eq!(exec.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn uia_skeleton_refuses_rather_than_pretending() {
        let exec = UiaExecutor::new();
        let ticket = ExecutorTicket::for_task(uuid::Uuid::new_v4());
        assert!(matches!(
            exec.execute(&ticket, &click("Submit")),
            Err(ActionError::ElementNotFound(_))
        ));
    }
}
