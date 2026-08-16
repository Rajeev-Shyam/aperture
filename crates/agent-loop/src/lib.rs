//! The observe→plan→act controller (Doc 22 §3.3) — **v2 SKELETON**.
//!
//! What is REAL and tested here: the task state machine with Doc 22's
//! termination conditions (step cap, consecutive-error threshold, hard stop),
//! as pure logic — the part every later milestone leans on and the part that
//! must be right before a single click ever happens.
//!
//! What is deliberately ABSENT until the grilling (Doc 22 §12):
//! - the live loop driver (V2-M2) — it composes `screen-serializer` →
//!   the MCP gate (`agent_step` as a 5th tool on the EXISTING `aperture-mcp`
//!   plumbing, per the v2 kickoff) → `action-executor`;
//! - scoped allow (V2-M3) — extends v1's `previews.approved` content-bound
//!   store, not new machinery;
//! - error recovery shape (Q-V2-07) and the step-cap default's final value
//!   (Q-V2-04 — 50 is the [ASSUMPTION] encoded here).
//!
//! Invariants (Doc 22 §11): the hard stop can never be disabled and always
//! wins; every step is audited via `task-manager` regardless of scoped allow;
//! the loop never acts without a user-initiated task.

use aperture_contracts::agent::{ActionInstruction, AgentStatus, TaskState};

/// Doc 22 §3.3 defaults — [ASSUMPTION]s until Q-V2-04 resolves.
pub const DEFAULT_STEP_CAP: u32 = 50;
/// Consecutive action failures before the loop fails the task.
pub const DEFAULT_ERROR_THRESHOLD: u32 = 3;

/// Why the loop stopped — surfaced to the user + written to the task row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Claude returned `task_complete`.
    Complete,
    /// The user hit the hard stop (tray / pill / hotkey). Always honored.
    HardStop,
    /// `DEFAULT_ERROR_THRESHOLD` consecutive action failures.
    ErrorThreshold,
    /// The step cap was reached without completion.
    StepCap,
    /// Claude said it cannot proceed.
    CannotProceed(String),
    /// VRAM pressure forced a sidecar unload mid-task — graceful pause,
    /// resumable (Doc 22 §3.3), not a failure.
    VramPause,
}

/// The pure per-task state machine (Doc 22 §3.3). The driver feeds it events;
/// it answers with the next [`TaskState`] and whether the loop may continue.
/// All transitions route through [`aperture_task_manager::transition_is_legal`]
/// so the in-memory machine and the persistence boundary can never disagree.
#[derive(Debug)]
pub struct TaskStateMachine {
    task_id: uuid::Uuid,
    state: TaskState,
    step: u32,
    step_cap: u32,
    consecutive_errors: u32,
    error_threshold: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LoopError {
    #[error("illegal transition {from:?} -> {to:?}")]
    IllegalTransition { from: TaskState, to: TaskState },
    #[error("the task is in a terminal state ({0:?})")]
    Terminal(TaskState),
}

impl TaskStateMachine {
    pub fn new(task_id: uuid::Uuid, step_cap: u32, error_threshold: u32) -> Self {
        Self {
            task_id,
            state: TaskState::Idle,
            step: 0,
            step_cap,
            consecutive_errors: 0,
            error_threshold,
        }
    }

    pub fn task_id(&self) -> uuid::Uuid {
        self.task_id
    }
    pub fn state(&self) -> TaskState {
        self.state
    }
    pub fn step(&self) -> u32 {
        self.step
    }

    fn transition(&mut self, to: TaskState) -> Result<(), LoopError> {
        if self.state.is_terminal() {
            return Err(LoopError::Terminal(self.state));
        }
        if !aperture_task_manager::transition_is_legal(self.state, to) {
            return Err(LoopError::IllegalTransition { from: self.state, to });
        }
        self.state = to;
        Ok(())
    }

    /// The user started the task (Doc 22 locked decision 4 — only the user
    /// ever starts one).
    pub fn start(&mut self) -> Result<(), LoopError> {
        self.transition(TaskState::Running)
    }

    /// The hard stop (locked decision 5): always legal from any live state,
    /// terminal immediately, never refused.
    pub fn hard_stop(&mut self) -> Result<StopReason, LoopError> {
        self.transition(TaskState::Cancelled)?;
        Ok(StopReason::HardStop)
    }

    /// Graceful pause (VRAM pressure / `need_clarification`). Resumable.
    pub fn pause(&mut self) -> Result<(), LoopError> {
        self.transition(TaskState::Paused)
    }

    /// Resume from a pause.
    pub fn resume(&mut self) -> Result<(), LoopError> {
        self.transition(TaskState::Running)
    }

    /// Account one completed step and its outcome; returns `Some(StopReason)`
    /// when a termination condition fired (the driver then transitions).
    ///
    /// The error threshold counts CONSECUTIVE failures (Doc 22 §3.3
    /// [ASSUMPTION]); any success resets it. Q-V2-07 may add a Claude-retry
    /// path before the threshold — that changes the driver, not this counting.
    pub fn record_step(&mut self, action_failed: bool) -> Option<StopReason> {
        self.step += 1;
        if action_failed {
            self.consecutive_errors += 1;
        } else {
            self.consecutive_errors = 0;
        }
        if self.consecutive_errors >= self.error_threshold {
            return Some(StopReason::ErrorThreshold);
        }
        if self.step >= self.step_cap {
            return Some(StopReason::StepCap);
        }
        None
    }

    /// Map Claude's per-step status onto loop control (Doc 22 §5).
    pub fn apply_instruction(&self, instruction: &ActionInstruction) -> Option<StopReason> {
        match instruction.status {
            AgentStatus::Continue => None,
            AgentStatus::TaskComplete => Some(StopReason::Complete),
            // Pauses are driver concerns (surface the question / chip); the
            // machine stays Running until the driver calls pause().
            AgentStatus::NeedClarification => None,
            AgentStatus::CannotProceed => {
                Some(StopReason::CannotProceed(instruction.reasoning.clone()))
            }
        }
    }

    /// Terminate with the given reason → the matching terminal state.
    pub fn finish(&mut self, reason: &StopReason) -> Result<TaskState, LoopError> {
        let to = match reason {
            StopReason::Complete => TaskState::Complete,
            StopReason::HardStop => TaskState::Cancelled,
            StopReason::ErrorThreshold | StopReason::StepCap | StopReason::CannotProceed(_) => {
                TaskState::Failed
            }
            StopReason::VramPause => TaskState::Paused,
        };
        self.transition(to)?;
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::agent::AgentConfidence;

    fn machine() -> TaskStateMachine {
        TaskStateMachine::new(uuid::Uuid::new_v4(), DEFAULT_STEP_CAP, DEFAULT_ERROR_THRESHOLD)
    }

    fn instruction(status: AgentStatus) -> ActionInstruction {
        ActionInstruction {
            status,
            reasoning: "because".into(),
            action: None,
            step_summary: None,
            confidence: AgentConfidence::High,
        }
    }

    #[test]
    fn happy_path_idle_running_complete() {
        let mut m = machine();
        m.start().unwrap();
        assert_eq!(m.state(), TaskState::Running);
        assert_eq!(m.record_step(false), None);
        let stop = m
            .apply_instruction(&instruction(AgentStatus::TaskComplete))
            .expect("complete stops");
        assert_eq!(m.finish(&stop).unwrap(), TaskState::Complete);
        // Terminal: nothing moves it again.
        assert!(matches!(m.start(), Err(LoopError::Terminal(_))));
    }

    #[test]
    fn three_consecutive_failures_stop_the_loop_but_a_success_resets() {
        let mut m = machine();
        m.start().unwrap();
        assert_eq!(m.record_step(true), None);
        assert_eq!(m.record_step(true), None);
        assert_eq!(m.record_step(false), None, "success resets the streak");
        assert_eq!(m.record_step(true), None);
        assert_eq!(m.record_step(true), None);
        assert_eq!(m.record_step(true), Some(StopReason::ErrorThreshold));
        assert_eq!(m.finish(&StopReason::ErrorThreshold).unwrap(), TaskState::Failed);
    }

    #[test]
    fn step_cap_fires_at_the_configured_bound() {
        let mut m = TaskStateMachine::new(uuid::Uuid::new_v4(), 3, DEFAULT_ERROR_THRESHOLD);
        m.start().unwrap();
        assert_eq!(m.record_step(false), None);
        assert_eq!(m.record_step(false), None);
        assert_eq!(m.record_step(false), Some(StopReason::StepCap));
    }

    #[test]
    fn hard_stop_always_wins_and_is_final() {
        let mut m = machine();
        m.start().unwrap();
        m.pause().unwrap();
        // Hard stop from PAUSED is legal (locked decision 5: always available).
        assert_eq!(m.hard_stop().unwrap(), StopReason::HardStop);
        assert_eq!(m.state(), TaskState::Cancelled);
        assert!(matches!(m.resume(), Err(LoopError::Terminal(_))));
    }

    #[test]
    fn vram_pause_is_resumable_not_terminal() {
        let mut m = machine();
        m.start().unwrap();
        assert_eq!(m.finish(&StopReason::VramPause).unwrap(), TaskState::Paused);
        m.resume().unwrap();
        assert_eq!(m.state(), TaskState::Running);
    }

    #[test]
    fn cannot_proceed_carries_claudes_reason() {
        let m = machine();
        let stop = m
            .apply_instruction(&ActionInstruction {
                status: AgentStatus::CannotProceed,
                reasoning: "the site requires a login I don't have".into(),
                action: None,
                step_summary: None,
                confidence: AgentConfidence::High,
            })
            .expect("stops");
        assert_eq!(
            stop,
            StopReason::CannotProceed("the site requires a login I don't have".into())
        );
    }
}
