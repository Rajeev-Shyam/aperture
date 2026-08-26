//! The agent's "hands" (Doc 22 §3.1) — **V2-M0: the real UIA backend**.
//!
//! Wraps Win32/UIA behind one seam so `agent-loop` is testable without a
//! desktop. [`UiaExecutor`] is the backend the V2-M0 gate drives
//! (`gates/tests/v2m0_uia_executor.rs`: a real Notepad window); Q-V2-01 (is
//! fuzzy UIA label matching sufficient?) is answered by that gate and the
//! pure [`grounding`] tests, not assumed here.
//!
//! ## Safety constraints (locked decisions, Doc 22 §11; Doc 24 §K)
//! - **UI-only action surface, forever** (Doc 24 #52): no filesystem writes,
//!   no registry, no network, no shell, **no process spawn** — `launch` is a
//!   simulated Start-menu search (Win key, type, Enter). This crate has no
//!   such dependencies and `xtask lint-emitters` denies the spawn/socket
//!   surface here; adding one is a review flag.
//! - **Only an active, user-initiated agent loop may act**: every call takes an
//!   [`ExecutorTicket`], minted only by `agent-loop` — see the lint note on
//!   [`ExecutorTicket::for_task`] (Doc 24 F2).
//! - **Exclusions are checked before acting** (Doc 22 §4.3, Doc 24 #49): the
//!   executor asks its [`ExclusionProbe`] about the foreground window on every
//!   call; a hit is `ActionError::Excluded` and the loop pauses and notifies.
//! - **Elevated windows are refused** (Doc 24 #50): `ActionError::Elevated`;
//!   the run-as-admin prompt is the loop's UI, not the executor's.
//! - **The hard stop wins** (locked decision 5): a raised stop flag makes
//!   every call `ActionError::Stopped` before anything is touched.
//!
//! Policy helpers the loop consults *before* dispatching live in [`risk`]
//! (consequential-action keywords, Doc 24 #47/#51; reversibility, #54).

pub mod grounding;
pub mod keys;
pub mod platform;
pub mod risk;
pub mod uia;

use aperture_contracts::agent::{ActionError, AgentAction};

pub use platform::{foreground_window, list_open_windows, read_document_text};
pub use risk::{consequential_reason, reversibility, Reversibility};
pub use uia::{close_windows, UiaExecutor};

/// Proof that an action originates from a user-initiated task loop
/// (Doc 22 §3.1 safety constraint). Carries the task id for the audit row.
#[derive(Debug, Clone)]
pub struct ExecutorTicket {
    task_id: uuid::Uuid,
}

impl ExecutorTicket {
    /// Minted by `agent-loop` when a task enters RUNNING; the ticket dies with
    /// the task.
    ///
    /// **Enforcement (Doc 24 F2):** Rust has no cross-crate `pub(crate)`, so
    /// the capability is enforced by `xtask lint-emitters` — the literal
    /// `ExecutorTicket::for_task(` may appear only under `crates/agent-loop/src`
    /// and this crate's own `src` (its tests); anywhere else under
    /// `crates/*/src` or `src-tauri/src` fails CI with "executor ticket minted
    /// outside agent-loop (Doc 24 F2)". The `gates` harness is lint-exempt by
    /// design (it drives the executor directly, on-target).
    pub fn for_task(task_id: uuid::Uuid) -> Self {
        Self { task_id }
    }

    pub fn task_id(&self) -> uuid::Uuid {
        self.task_id
    }
}

/// A top-level window as the executor sees it (`list_open_windows`,
/// `foreground_window`, [`ActionOutcome::new_windows`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowInfo {
    /// Raw `HWND` (valid only while the window lives).
    pub hwnd: isize,
    /// Window title (never empty for listed windows).
    pub title: String,
    /// Lowercased process image name (`"notepad.exe"`), when readable.
    pub process: Option<String>,
    /// Window class — the exclusion key capture also uses (doc 05 §4).
    pub window_class: Option<String>,
}

/// "Is this window excluded?" — the seam over capture's `ExclusionList`
/// (Doc 22 §4.3, Doc 24 #49). src-tauri implements it; returns the matching
/// rule's label (what the pause notification shows), or `None`.
pub trait ExclusionProbe: Send + Sync {
    fn excluded_label(
        &self,
        process: Option<&str>,
        window_class: Option<&str>,
        title: Option<&str>,
    ) -> Option<String>;
}

/// No exclusion rules at all (gates, spikes).
pub struct NoExclusions;

impl ExclusionProbe for NoExclusions {
    fn excluded_label(&self, _: Option<&str>, _: Option<&str>, _: Option<&str>) -> Option<String> {
        None
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
    /// Top-level windows that appeared during the action — the undo hook for
    /// `launch` (Doc 24 #54: "closed an app it opened" is reversible).
    #[serde(default)]
    pub new_windows: Vec<WindowInfo>,
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
            Ok(ActionOutcome {
                description: "clicked Submit".into(),
                focused_element: None,
                new_windows: Vec::new(),
            }),
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

    /// `new_windows` is additive on the wire (doc 15 §6): an outcome persisted
    /// before V2-M0 still parses.
    #[test]
    fn outcome_new_windows_defaults_on_the_wire() {
        let old = serde_json::json!({ "description": "clicked", "focused_element": null });
        let parsed: ActionOutcome = serde_json::from_value(old).expect("pre-M0 outcome parses");
        assert!(parsed.new_windows.is_empty());
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back["new_windows"], serde_json::json!([]));
    }

    #[test]
    fn no_exclusions_never_excludes() {
        assert_eq!(NoExclusions.excluded_label(Some("x.exe"), Some("Cls"), Some("T")), None);
    }
}
