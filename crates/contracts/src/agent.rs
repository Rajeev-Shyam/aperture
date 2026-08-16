//! Contract 6 — the v2 Agent Execution Layer types (Doc 22, v2 skeleton).
//!
//! **Status: v2 SKELETON.** These types encode Doc 22's locked decisions
//! (§11) and its §5 response schema so the four v2 crates (`action-executor`,
//! `screen-serializer`, `agent-loop`, `task-manager`) compose against one
//! shared contract from day one. Behavior marked with a Q-V2 number is BLOCKED
//! on that open question (Doc 22 §12) and must not be implemented before the
//! grilling resolves it.
//!
//! Invariants carried from v1, verbatim (Doc 22 §11 + the kickoff doc):
//! - Claude decides, local executes — neither acts alone (§2).
//! - The executor is NOT an emitter: it never touches the network (two-emitter
//!   rule, doc 13 §2). Its action surface is UI-only (no fs/registry/shell).
//! - Every step is audited (payload hash + action + result) regardless of
//!   scoped allow. The hard stop can never be disabled.

use serde::{Deserialize, Serialize};

/// Claude's per-step status (Doc 22 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Keep looping: `action` describes the next step.
    Continue,
    /// The task is done; the loop terminates gracefully.
    TaskComplete,
    /// Pause and surface Claude's question to the user (Doc 22 §5).
    NeedClarification,
    /// Terminate gracefully and surface the reason.
    CannotProceed,
}

/// The action verb set (Doc 22 §5) — UI-only by locked decision 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Click,
    Type,
    Key,
    Launch,
    SwitchWindow,
    Scroll,
    Wait,
    None,
}

/// Scroll direction for [`ActionType::Scroll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Claude's confidence tag; `Low` pauses for a user confirmation chip
/// (Doc 22 §5, [ASSUMPTION] — safer to ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfidence {
    High,
    Medium,
    Low,
}

/// Optional pixel fallback coordinates (Doc 22 §6 — used only when the UIA
/// label match fails; accuracy from a 768 px downscale is Q-V2-01/[VERIFY]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelCoords {
    pub x: i32,
    pub y: i32,
}

/// One action instruction inside Claude's step response (Doc 22 §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    #[serde(rename = "type")]
    pub action_type: ActionType,
    /// Element label or window name (click / switch_window).
    #[serde(default)]
    pub target: Option<String>,
    /// Text to type or key combo (type / key).
    #[serde(default)]
    pub value: Option<String>,
    /// Scroll direction (scroll only).
    #[serde(default)]
    pub direction: Option<ScrollDirection>,
    /// Scroll amount in notches (scroll only).
    #[serde(default)]
    pub amount: Option<u32>,
    /// Pixel fallback (Doc 22 §6) — only consulted when UIA matching fails.
    #[serde(default)]
    pub coords: Option<PixelCoords>,
}

/// The structured JSON Claude must return at every agent step (Doc 22 §5).
/// Enforced via the system prompt; parse failures count toward the error
/// threshold (Q-V2-07 decides the retry shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInstruction {
    pub status: AgentStatus,
    /// One sentence — why this action.
    pub reasoning: String,
    #[serde(default)]
    pub action: Option<AgentAction>,
    /// One-sentence rolling summary of all steps so far (Doc 22 §3.2 —
    /// becomes the next payload's `prior_steps_summary`).
    #[serde(default)]
    pub step_summary: Option<String>,
    pub confidence: AgentConfidence,
}

/// Task lifecycle states (Doc 22 §3.3). Legal transitions are enforced by
/// `agent-loop`'s state machine; `task-manager` persists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Idle,
    Running,
    Paused,
    Complete,
    Failed,
    Cancelled,
}

impl TaskState {
    /// The stable string persisted in `tasks.status` (Doc 22 §3.4).
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Idle => "idle",
            TaskState::Running => "running",
            TaskState::Paused => "paused",
            TaskState::Complete => "complete",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }

    /// Terminal states never transition again (the hard stop writes
    /// `Cancelled` exactly once — locked decision 5).
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Complete | TaskState::Failed | TaskState::Cancelled)
    }
}

/// Executor failure modes (Doc 22 §3.1). The loop maps these onto the error
/// threshold and the Q-V2-07 recovery path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum ActionError {
    #[error("element not found: {0}")]
    ElementNotFound(String),
    #[error("element not interactable: {0}")]
    NotInteractable(String),
    #[error("timed out waiting for the screen to settle")]
    Timeout,
    /// UAC-elevated window (Task Manager, installers): Q-V2-09 decides hard
    /// block vs "run as admin" prompt — until then, always a hard pause.
    #[error("target window is UAC-elevated: {0}")]
    Elevated(String),
    /// The focused window matches the user's exclusion list: the loop pauses
    /// and notifies rather than acting blind (Doc 22 §4.3, Q-V2-03).
    #[error("target window is excluded: {0}")]
    Excluded(String),
}

/// Per-step outcome persisted in `task_steps.result` (Doc 22 §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepResult {
    Success,
    Failure,
    Skipped,
}

impl StepResult {
    /// The stable string persisted in `task_steps.result`.
    pub fn as_str(self) -> &'static str {
        match self {
            StepResult::Success => "success",
            StepResult::Failure => "failure",
            StepResult::Skipped => "skipped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doc 22 §5's example shape must parse verbatim — this is the wire
    /// contract the system prompt promises Claude.
    #[test]
    fn doc22_response_schema_round_trips() {
        let wire = serde_json::json!({
            "status": "continue",
            "reasoning": "the form needs the name filled first",
            "action": {
                "type": "type",
                "target": "Name field",
                "value": "Rajeev",
            },
            "step_summary": "opened the form and started filling it",
            "confidence": "high"
        });
        let parsed: ActionInstruction = serde_json::from_value(wire).expect("parses");
        assert_eq!(parsed.status, AgentStatus::Continue);
        let action = parsed.action.as_ref().expect("action present");
        assert_eq!(action.action_type, ActionType::Type);
        assert_eq!(action.value.as_deref(), Some("Rajeev"));
        // And back out — additive-only compatibility (doc 15 §6).
        let back = serde_json::to_value(&parsed).expect("serializes");
        assert_eq!(back["status"], "continue");
        assert_eq!(back["action"]["type"], "type");
    }

    #[test]
    fn task_complete_needs_no_action() {
        let parsed: ActionInstruction = serde_json::from_value(serde_json::json!({
            "status": "task_complete",
            "reasoning": "the form was submitted",
            "confidence": "high"
        }))
        .expect("parses without action/step_summary");
        assert_eq!(parsed.status, AgentStatus::TaskComplete);
        assert!(parsed.action.is_none());
    }

    #[test]
    fn terminal_states_are_terminal() {
        for s in [TaskState::Complete, TaskState::Failed, TaskState::Cancelled] {
            assert!(s.is_terminal());
        }
        for s in [TaskState::Idle, TaskState::Running, TaskState::Paused] {
            assert!(!s.is_terminal());
        }
    }
}
