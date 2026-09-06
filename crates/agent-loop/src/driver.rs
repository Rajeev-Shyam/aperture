//! The live loop driver (Doc 22 §3.3, V2-M2/M3/M5) — everything the
//! observe→plan→act cycle needs that is NOT I/O.
//!
//! The shell (src-tauri) owns the I/O: it observes the screen through
//! `aperture_capture::CaptureSubsystem::observe_now`, serializes through
//! `aperture_screen_serializer`, carries Claude's instruction in over the
//! MCP gate (`aperture_agent_step`, a 5th tool on the EXISTING plumbing — v2
//! kickoff §1), shows the user the surfaces (Doc 22 §9, decision #53) and
//! waits for their decisions. This driver is the pure policy in between, so it
//! can be tested to the last branch with `ScriptedExecutor` + an in-memory
//! `TaskManager`.
//!
//! Owner decisions encoded here (Doc 24 §K):
//! - **#47 / #51** — confirm only risky-looking actions: `risk::consequential_reason`
//!   on the label, independent of Claude's confidence; `confidence: low` also
//!   asks (Doc 22 §5 [ASSUMPTION]).
//! - **#48** — approve ONCE per task (the scoped allow of V2-M3); every step is
//!   still audited and cancellable.
//! - **#49** — an excluded window in front ⇒ **pause and notify**, never act.
//! - **#50** — a UAC-elevated window ⇒ pause with the admin prompt (the shell
//!   renders it; see `PauseReason::Elevated`).
//! - **#54** — reversibility is classified per action and the windows a task
//!   opened are remembered, so "undo" can offer exactly what it can deliver.
//! - **F2** — the `ExecutorTicket` is minted here and only here (xtask
//!   `lint-emitters` refuses it anywhere else).
//! - Locked decision 5: the hard stop always wins — it is a flag the executor
//!   checks before every action AND a transition this driver never refuses.
//! - Locked decision 6: a step row is written for every executed, skipped or
//!   refused action regardless of approval state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aperture_action_executor::{
    reversibility, risk, ActionExecutor, ActionOutcome, ExecutorTicket, Reversibility, WindowInfo,
};
use aperture_contracts::agent::{
    ActionError, ActionInstruction, AgentAction, AgentConfidence, AgentStatus, StepResult,
    TaskState,
};
use aperture_screen_serializer::LastAction;
use aperture_task_manager::{StepRecord, Task, TaskError, TaskManager};

use crate::{StopReason, TaskStateMachine, DEFAULT_ERROR_THRESHOLD, DEFAULT_STEP_CAP};

/// Tunables (Doc 22 §3.3 [ASSUMPTION]s; Q-V2-04 keeps 50 as the default and
/// makes it a setting — `agent.step_cap`).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub step_cap: u32,
    pub error_threshold: u32,
    /// Pause for confirmation when Claude says `confidence: low` (Doc 22 §5).
    pub confirm_low_confidence: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            step_cap: DEFAULT_STEP_CAP,
            error_threshold: DEFAULT_ERROR_THRESHOLD,
            confirm_low_confidence: true,
        }
    }
}

/// Why the loop is waiting on the user (rendered by the shell, Doc 22 §9).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseReason {
    /// The task itself has not been approved yet (decision #48: once per task).
    Approval,
    /// A consequential-looking or low-confidence action awaits Approve / Skip /
    /// Stop (decision #47).
    Confirm { reason: String, action: AgentAction },
    /// Claude asked a question (Doc 22 §5 `need_clarification`).
    Clarification { question: String },
    /// The window in front is on the exclusion list (decision #49).
    Excluded { label: String },
    /// The window in front is UAC-elevated (decision #50).
    Elevated { window: String },
    /// VRAM pressure forced a sidecar unload (Doc 22 §3.3) — resumable.
    Vram,
}

/// One line of the live step log (decision #53) — also what the Dashboard's
/// task history renders from `task_steps`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepLogEntry {
    pub step_number: u32,
    pub summary: String,
    pub result: String,
    pub reversibility: String,
    pub ts: i64,
}

/// What the driver decided about an incoming instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Disposition {
    /// Run it now.
    Execute(AgentAction),
    /// Ask the user first (stored as the pending action; see [`AgentDriver::confirm`]).
    NeedsConfirm { reason: String },
    /// Surface Claude's question; resume with [`AgentDriver::answer`].
    Clarify { question: String },
    /// Nothing to do (status `continue` with no action / `none`).
    Noop,
    /// The loop ended.
    Finished(StopReason),
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("{0}")]
    Loop(#[from] crate::LoopError),
    #[error("{0}")]
    Task(#[from] TaskError),
    #[error("no action is awaiting confirmation")]
    NothingPending,
    #[error("the task is not paused")]
    NotPaused,
}

/// Per-task driver state. One task at a time (Doc 22 §14).
pub struct AgentDriver {
    task: Task,
    machine: TaskStateMachine,
    ticket: ExecutorTicket,
    executor: Arc<dyn ActionExecutor>,
    tasks: Arc<TaskManager>,
    config: LoopConfig,
    /// The hard-stop flag shared with the executor (locked decision 5).
    stop: Arc<AtomicBool>,
    approved: bool,
    pending: Option<ActionInstruction>,
    pause: Option<PauseReason>,
    /// SHA-256 of the last payload staged for Claude — the audit datum the
    /// next step row carries (locked decision 6).
    last_payload_hash: Option<String>,
    prior_summary: Option<String>,
    last_action: Option<LastAction>,
    log: Vec<StepLogEntry>,
    /// Windows that appeared during this task's actions (decision #54 undo).
    opened_windows: Vec<WindowInfo>,
    stop_reason: Option<StopReason>,
}

impl AgentDriver {
    /// Adopt a freshly created (`Idle`) task. `stop` is the flag the executor
    /// was built with, so a hard stop reaches it without going through this
    /// driver's lock.
    pub fn new(
        task: Task,
        executor: Arc<dyn ActionExecutor>,
        tasks: Arc<TaskManager>,
        stop: Arc<AtomicBool>,
        config: LoopConfig,
    ) -> Self {
        let ticket = ExecutorTicket::for_task(task.id);
        let machine = TaskStateMachine::new(task.id, config.step_cap, config.error_threshold);
        Self {
            task,
            machine,
            ticket,
            executor,
            tasks,
            config,
            stop,
            approved: false,
            pending: None,
            pause: Some(PauseReason::Approval),
            last_payload_hash: None,
            prior_summary: None,
            last_action: None,
            log: Vec::new(),
            opened_windows: Vec::new(),
            stop_reason: None,
        }
    }

    // ---- read side ------------------------------------------------------

    pub fn task(&self) -> &Task {
        &self.task
    }
    pub fn task_id(&self) -> uuid::Uuid {
        self.task.id
    }
    pub fn state(&self) -> TaskState {
        self.machine.state()
    }
    pub fn step(&self) -> u32 {
        self.machine.step()
    }
    pub fn is_approved(&self) -> bool {
        self.approved
    }
    pub fn pause(&self) -> Option<&PauseReason> {
        self.pause.as_ref()
    }
    pub fn log(&self) -> &[StepLogEntry] {
        &self.log
    }
    pub fn prior_summary(&self) -> Option<&str> {
        self.prior_summary.as_deref()
    }
    pub fn last_action(&self) -> Option<&LastAction> {
        self.last_action.as_ref()
    }
    pub fn opened_windows(&self) -> &[WindowInfo] {
        &self.opened_windows
    }
    pub fn stop_reason(&self) -> Option<&StopReason> {
        self.stop_reason.as_ref()
    }
    pub fn is_terminal(&self) -> bool {
        self.machine.state().is_terminal()
    }
    pub fn step_cap(&self) -> u32 {
        self.config.step_cap
    }
    /// The executor's settle hint (Doc 22 §2, 300–800 ms band).
    pub fn settle_hint(&self) -> std::time::Duration {
        self.executor.settle_hint()
    }

    // ---- lifecycle ------------------------------------------------------

    /// The user allowed the task (decision #48 — once, here). Idle → Running.
    /// Persist FIRST, then move the in-memory machine, so a DB failure leaves
    /// both where they were (a second Approve then simply retries).
    pub fn approve(&mut self, now_ms: i64) -> Result<(), DriverError> {
        if self.approved && self.machine.state() == TaskState::Running {
            return Ok(()); // idempotent: a double click is not an error
        }
        self.task = self.tasks.transition(self.task.id, TaskState::Running, now_ms, None)?;
        self.machine.start()?;
        self.approved = true;
        self.pause = None;
        Ok(())
    }

    /// The user denied the task before it started. Idle → Cancelled.
    pub fn deny(&mut self, now_ms: i64) -> Result<(), DriverError> {
        self.finish(StopReason::HardStop, now_ms)
    }

    /// Locked decision 5. Sets the executor's flag first (so an action that
    /// has not started yet is refused even if this call waits on nothing),
    /// then records and transitions. Never refused from a live state; a
    /// no-op from a terminal one.
    pub fn hard_stop(&mut self, now_ms: i64) -> Result<(), DriverError> {
        self.stop.store(true, Ordering::SeqCst);
        if self.is_terminal() {
            return Ok(());
        }
        self.record_row("hard_stop", None, None, StepResult::Skipped, Some("user hard stop"), now_ms);
        self.finish(StopReason::HardStop, now_ms)
    }

    /// Record that a payload was staged for Claude (the hash rides on the next
    /// step row) and the summary Claude will see next.
    pub fn observed(&mut self, payload_hash: String) {
        self.last_payload_hash = Some(payload_hash);
    }

    /// Decide what to do with Claude's instruction (Doc 22 §5 + decision #47).
    /// Pure; execution is a separate call so the shell can ask the user in
    /// between without holding anything.
    pub fn classify(&mut self, instruction: &ActionInstruction, now_ms: i64) -> Result<Disposition, DriverError> {
        if self.stop.load(Ordering::SeqCst) && !self.is_terminal() {
            self.hard_stop(now_ms)?;
            return Ok(Disposition::Finished(StopReason::HardStop));
        }
        if self.is_terminal() {
            return Ok(Disposition::Finished(
                self.stop_reason.clone().unwrap_or(StopReason::HardStop),
            ));
        }
        if let Some(summary) = &instruction.step_summary {
            self.prior_summary = Some(summary.clone());
        }
        if let Some(reason) = self.machine.apply_instruction(instruction) {
            // Complete / cannot_proceed: audit the decision, then end.
            self.record_row(
                match instruction.status {
                    AgentStatus::TaskComplete => "task_complete",
                    _ => "cannot_proceed",
                },
                None,
                None,
                StepResult::Skipped,
                Some(&instruction.reasoning),
                now_ms,
            );
            self.finish_with_summary(reason.clone(), instruction.step_summary.as_deref(), now_ms)?;
            return Ok(Disposition::Finished(reason));
        }
        if instruction.status == AgentStatus::NeedClarification {
            let question = instruction.reasoning.clone();
            self.machine.pause()?;
            self.task = self.tasks.transition(self.task.id, TaskState::Paused, now_ms, None)?;
            self.pause = Some(PauseReason::Clarification { question: question.clone() });
            return Ok(Disposition::Clarify { question });
        }
        // A chip the user has not answered yet stays the question on the
        // table: a later instruction (Claude re-calling after the bounded
        // wait) must not overwrite it or slip past it (review 2026-08-22).
        if let (Some(_), Some(PauseReason::Confirm { reason, .. })) = (&self.pending, &self.pause) {
            return Ok(Disposition::NeedsConfirm { reason: reason.clone() });
        }
        let Some(action) = instruction.action.clone() else {
            self.note_noop(&instruction.reasoning, now_ms);
            return Ok(Disposition::Noop);
        };
        if matches!(action.action_type, aperture_contracts::agent::ActionType::None) {
            self.note_noop(&instruction.reasoning, now_ms);
            return Ok(Disposition::Noop);
        }
        let risky = risk::consequential_reason(&action);
        let low = self.config.confirm_low_confidence
            && instruction.confidence == AgentConfidence::Low;
        if risky.is_some() || low {
            let reason = match (risky, low) {
                (Some(r), _) => format!("looks consequential: \"{r}\""),
                (None, _) => "Claude is not confident about this step".to_string(),
            };
            self.pending = Some(instruction.clone());
            self.pause = Some(PauseReason::Confirm { reason: reason.clone(), action });
            return Ok(Disposition::NeedsConfirm { reason });
        }
        Ok(Disposition::Execute(action))
    }

    /// The user answered the confirmation chip. `allow` ⇒ the pending action
    /// is returned for [`AgentDriver::execute`]; otherwise it is audited as
    /// skipped and the loop continues with the next observation.
    pub fn confirm(&mut self, allow: bool, now_ms: i64) -> Result<Option<ActionInstruction>, DriverError> {
        let pending = self.pending.take().ok_or(DriverError::NothingPending)?;
        self.pause = None;
        if allow {
            return Ok(Some(pending));
        }
        let action = pending.action.as_ref();
        self.record_row(
            action.map(|a| action_type_str(a)).unwrap_or("none"),
            action.and_then(|a| a.target.as_deref()),
            action.and_then(|a| a.value.as_deref()),
            StepResult::Skipped,
            Some("user skipped"),
            now_ms,
        );
        self.last_action = action.map(|a| LastAction {
            action_type: action_type_str(a).to_string(),
            target: a.target.clone().unwrap_or_default(),
            result: "skipped by user".to_string(),
        });
        // A skip is not an executor failure; it does not count toward the
        // error threshold, but it is a step (Doc 22 §3.3 step cap).
        if let Some(stop) = self.machine.record_step(false) {
            self.finish(stop.clone(), now_ms)?;
        }
        Ok(None)
    }

    /// The user answered Claude's question; the answer rides into the next
    /// payload's `prior_steps_summary` so Claude sees it.
    pub fn answer(&mut self, answer: &str, now_ms: i64) -> Result<(), DriverError> {
        match self.pause {
            Some(PauseReason::Clarification { .. }) => {}
            _ => return Err(DriverError::NotPaused),
        }
        let prior = self.prior_summary.take().unwrap_or_default();
        self.prior_summary = Some(format!("{prior}\nUser answered: {answer}").trim().to_string());
        self.resume(now_ms)
    }

    /// Resume after an excluded/elevated/VRAM pause (decision #49/#50).
    /// Persist first (see `approve`).
    pub fn resume(&mut self, now_ms: i64) -> Result<(), DriverError> {
        if self.machine.state() != TaskState::Paused {
            return Err(DriverError::NotPaused);
        }
        self.task = self.tasks.transition(self.task.id, TaskState::Running, now_ms, None)?;
        self.machine.resume()?;
        self.pause = None;
        Ok(())
    }

    /// Perform one action through the executor and account for it (locked
    /// decision 6: the row is written whatever happened). Returns the outcome
    /// for the shell to echo; pauses/terminal transitions are applied here.
    pub fn execute(
        &mut self,
        instruction: &ActionInstruction,
        action: &AgentAction,
        now_ms: i64,
    ) -> Result<Result<ActionOutcome, ActionError>, DriverError> {
        let outcome = self.executor.execute(&self.ticket, action);
        let kind = action_type_str(action);
        let (result, result_text) = match &outcome {
            Ok(o) => (StepResult::Success, o.description.clone()),
            Err(e) => (StepResult::Failure, e.to_string()),
        };
        self.record_row(
            kind,
            action.target.as_deref(),
            action.value.as_deref(),
            result,
            Some(&instruction.reasoning),
            now_ms,
        );
        self.last_action = Some(LastAction {
            action_type: kind.to_string(),
            target: action.target.clone().unwrap_or_default(),
            result: result_text,
        });
        if let Ok(o) = &outcome {
            self.opened_windows.extend(o.new_windows.iter().cloned());
        }
        match &outcome {
            Err(ActionError::Stopped) => {
                self.finish(StopReason::HardStop, now_ms)?;
                return Ok(outcome);
            }
            Err(ActionError::Excluded(label)) => {
                self.pause_with(PauseReason::Excluded { label: label.clone() }, now_ms)?;
                return Ok(outcome);
            }
            Err(ActionError::Elevated(window)) => {
                self.pause_with(PauseReason::Elevated { window: window.clone() }, now_ms)?;
                return Ok(outcome);
            }
            _ => {}
        }
        if let Some(stop) = self.machine.record_step(outcome.is_err()) {
            self.finish(stop, now_ms)?;
        }
        Ok(outcome)
    }

    /// VRAM pressure forced a sidecar unload mid-task (Doc 22 §3.3): graceful,
    /// resumable pause.
    pub fn vram_pause(&mut self, now_ms: i64) -> Result<(), DriverError> {
        self.pause_with(PauseReason::Vram, now_ms)
    }

    /// Pause for a reason the shell observed outside an action — an excluded
    /// window in front at OBSERVATION time (decision #49: the agent must not
    /// even look), or VRAM pressure.
    pub fn pause_for(&mut self, reason: PauseReason, now_ms: i64) -> Result<(), DriverError> {
        self.pause_with(reason, now_ms)
    }

    /// A turn that does nothing (status `continue` with no action / `none`,
    /// or a repeat observation without an instruction) is still a step:
    /// audited, and counted toward the step cap so a planner that only ever
    /// looks cannot release screens forever (RK-V2-03; review 2026-08-22).
    pub fn note_noop(&mut self, reasoning: &str, now_ms: i64) {
        if self.is_terminal() {
            return;
        }
        self.record_row("none", None, None, StepResult::Skipped, Some(reasoning), now_ms);
        if let Some(stop) = self.machine.record_step(false) {
            let _ = self.finish(stop, now_ms);
        }
    }

    /// The shell caught a panic on the executor thread (08-22 review): the
    /// driver survived but the step did not. A failed row, then the task ends
    /// Failed via `CannotProceed` — the errors terminal path — never
    /// `HardStop`, which is reserved for the user (locked decision 5).
    pub fn fail_internal(&mut self, why: &str, now_ms: i64) {
        if self.is_terminal() {
            return;
        }
        self.record_row("internal_error", None, None, StepResult::Failure, Some(why), now_ms);
        let _ = self.finish(StopReason::CannotProceed(format!("internal error: {why}")), now_ms);
    }

    /// Q-V2-07 [PROVISIONAL]: an instruction that did not parse is a failed
    /// step — audited and counted toward the consecutive-error threshold, so
    /// a planner stuck emitting garbage cannot loop forever.
    pub fn note_malformed(&mut self, error: &str, now_ms: i64) {
        if self.is_terminal() {
            return;
        }
        self.record_row("malformed_instruction", None, None, StepResult::Failure, Some(error), now_ms);
        self.last_action = Some(LastAction {
            action_type: "none".into(),
            target: String::new(),
            result: format!("your last instruction did not parse: {error}"),
        });
        if let Some(stop) = self.machine.record_step(true) {
            let _ = self.finish(stop, now_ms);
        }
    }

    /// Decision #54: which of the windows this task opened are still open —
    /// the honest undo offer ("close N windows the task opened"). The shell
    /// passes the current window list so this stays pure.
    pub fn undoable_windows<'a>(&'a self, open_now: &'a [WindowInfo]) -> Vec<&'a WindowInfo> {
        self.opened_windows
            .iter()
            .filter(|w| open_now.iter().any(|o| o.hwnd == w.hwnd))
            .collect()
    }

    // ---- internals ------------------------------------------------------

    fn pause_with(&mut self, reason: PauseReason, now_ms: i64) -> Result<(), DriverError> {
        if self.machine.state() == TaskState::Running {
            self.machine.pause()?;
            self.task = self.tasks.transition(self.task.id, TaskState::Paused, now_ms, None)?;
        }
        self.pause = Some(reason);
        Ok(())
    }

    fn finish(&mut self, reason: StopReason, now_ms: i64) -> Result<(), DriverError> {
        self.finish_with_summary(reason, None, now_ms)
    }

    fn finish_with_summary(
        &mut self,
        reason: StopReason,
        summary: Option<&str>,
        now_ms: i64,
    ) -> Result<(), DriverError> {
        if self.is_terminal() {
            return Ok(());
        }
        let to = self.machine.finish(&reason)?;
        let outcome = match (&reason, summary) {
            (StopReason::Complete, Some(s)) => Some(s.to_string()),
            (StopReason::Complete, None) => Some("task complete".to_string()),
            (StopReason::CannotProceed(why), _) => Some(format!("cannot proceed: {why}")),
            (StopReason::HardStop, _) => Some("stopped by user".to_string()),
            (StopReason::ErrorThreshold, _) => {
                Some("stopped: too many consecutive action failures".to_string())
            }
            (StopReason::StepCap, _) => Some("stopped: step cap reached".to_string()),
            (StopReason::VramPause, _) => None,
        };
        self.task = self.tasks.transition(self.task.id, to, now_ms, outcome.as_deref())?;
        if to.is_terminal() {
            self.stop_reason = Some(reason);
            self.pause = None;
            self.pending = None;
        } else {
            self.pause = Some(PauseReason::Vram);
        }
        Ok(())
    }

    fn record_row(
        &mut self,
        action_type: &str,
        target: Option<&str>,
        value: Option<&str>,
        result: StepResult,
        reasoning: Option<&str>,
        now_ms: i64,
    ) {
        let step_number = self.machine.step() + 1;
        let row = StepRecord {
            task_id: self.task.id,
            step_number,
            screen_payload_hash: self.last_payload_hash.clone(),
            action_type: Some(action_type.to_string()),
            action_target: target.map(str::to_string),
            action_value: value.map(str::to_string),
            result: Some(result),
            claude_reasoning: reasoning.map(str::to_string),
            timestamp: now_ms,
        };
        if let Err(e) = self.tasks.record_step(&row) {
            // Locked decision 6 says every step is audited; a write failure is
            // loud, not silent, and the shell surfaces it as an audit alert.
            tracing::error!(task = %self.task.id, error = %e, "task step audit write failed");
        }
        let rev = match target {
            Some(t) => reversibility(&AgentAction {
                action_type: parse_action_type(action_type),
                target: Some(t.to_string()),
                value: value.map(str::to_string),
                direction: None,
                amount: None,
                coords: None,
            }),
            None => Reversibility::Unknown,
        };
        self.log.push(StepLogEntry {
            step_number,
            summary: match target {
                Some(t) => format!("{action_type} \"{t}\""),
                None => action_type.to_string(),
            },
            result: result.as_str().to_string(),
            reversibility: format!("{rev:?}").to_lowercase(),
            ts: now_ms,
        });
    }
}

/// The stable wire string of an action verb (`task_steps.action_type`).
pub fn action_type_str(a: &AgentAction) -> &'static str {
    use aperture_contracts::agent::ActionType::*;
    match a.action_type {
        Click => "click",
        Type => "type",
        Key => "key",
        Launch => "launch",
        SwitchWindow => "switch_window",
        Scroll => "scroll",
        Wait => "wait",
        None => "none",
    }
}

fn parse_action_type(s: &str) -> aperture_contracts::agent::ActionType {
    use aperture_contracts::agent::ActionType::*;
    match s {
        "click" => Click,
        "type" => Type,
        "key" => Key,
        "launch" => Launch,
        "switch_window" => SwitchWindow,
        "scroll" => Scroll,
        "wait" => Wait,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_action_executor::ScriptedExecutor;
    use aperture_contracts::agent::ActionType;
    use aperture_db::Db;

    fn harness(
        outcomes: Vec<Result<ActionOutcome, ActionError>>,
    ) -> (AgentDriver, Arc<TaskManager>, Arc<AtomicBool>) {
        let tasks = Arc::new(TaskManager::new(Arc::new(Db::open_in_memory().unwrap())));
        let task = tasks.create_task("fill the form", 1_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let exec: Arc<dyn ActionExecutor> = Arc::new(ScriptedExecutor::new(outcomes));
        let d = AgentDriver::new(task, exec, Arc::clone(&tasks), Arc::clone(&stop), LoopConfig::default());
        (d, tasks, stop)
    }

    fn ok(desc: &str) -> Result<ActionOutcome, ActionError> {
        Ok(ActionOutcome { description: desc.into(), focused_element: None, new_windows: vec![] })
    }

    fn click(target: &str, confidence: AgentConfidence) -> ActionInstruction {
        ActionInstruction {
            status: AgentStatus::Continue,
            reasoning: "next field".into(),
            action: Some(AgentAction {
                action_type: ActionType::Click,
                target: Some(target.into()),
                value: None,
                direction: None,
                amount: None,
                coords: None,
            }),
            step_summary: Some("clicked things".into()),
            confidence,
        }
    }

    #[test]
    fn starts_paused_for_approval_then_runs_and_completes_with_audit_rows() {
        let (mut d, tasks, _) = harness(vec![ok("clicked Name"), ok("clicked Submit")]);
        assert_eq!(d.pause(), Some(&PauseReason::Approval));
        assert!(!d.is_approved());
        d.approve(2_000).unwrap();
        assert_eq!(d.state(), TaskState::Running);

        d.observed("hash-1".into());
        let i = click("Name field", AgentConfidence::High);
        let Disposition::Execute(a) = d.classify(&i, 3_000).unwrap() else { panic!("execute") };
        d.execute(&i, &a, 3_000).unwrap().unwrap();
        assert_eq!(d.step(), 1);
        assert_eq!(d.last_action().unwrap().result, "clicked Name");

        d.observed("hash-2".into());
        let done = ActionInstruction {
            status: AgentStatus::TaskComplete,
            reasoning: "form submitted".into(),
            action: None,
            step_summary: Some("filled and submitted the form".into()),
            confidence: AgentConfidence::High,
        };
        assert_eq!(d.classify(&done, 4_000).unwrap(), Disposition::Finished(StopReason::Complete));
        assert_eq!(d.state(), TaskState::Complete);
        let t = tasks.get_task(d.task_id()).unwrap();
        assert_eq!(t.status, TaskState::Complete);
        assert_eq!(t.outcome_summary.as_deref(), Some("filled and submitted the form"));
        let steps = tasks.steps(d.task_id()).unwrap();
        assert_eq!(steps.len(), 2, "one row per executed action + the completion");
        assert_eq!(steps[0].screen_payload_hash.as_deref(), Some("hash-1"));
        assert_eq!(steps[0].action_type.as_deref(), Some("click"));
        assert_eq!(steps[1].action_type.as_deref(), Some("task_complete"));
        assert_eq!(steps[1].screen_payload_hash.as_deref(), Some("hash-2"));
    }

    #[test]
    fn consequential_labels_need_confirmation_and_skip_is_audited() {
        let (mut d, tasks, _) = harness(vec![ok("clicked Delete")]);
        d.approve(1).unwrap();
        let i = click("Delete account", AgentConfidence::High);
        let disp = d.classify(&i, 2).unwrap();
        assert!(matches!(disp, Disposition::NeedsConfirm { .. }), "{disp:?}");
        assert!(matches!(d.pause(), Some(PauseReason::Confirm { .. })));
        // Skip ⇒ no execution, a skipped row, loop continues.
        assert!(d.confirm(false, 3).unwrap().is_none());
        assert_eq!(d.state(), TaskState::Running);
        let steps = tasks.steps(d.task_id()).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].result, Some(StepResult::Skipped));
        assert_eq!(steps[0].claude_reasoning.as_deref(), Some("user skipped"));
        // Approve path returns the instruction to execute.
        let disp = d.classify(&i, 4).unwrap();
        assert!(matches!(disp, Disposition::NeedsConfirm { .. }));
        let pending = d.confirm(true, 5).unwrap().expect("pending returned");
        let a = pending.action.clone().unwrap();
        d.execute(&pending, &a, 5).unwrap().unwrap();
        assert_eq!(tasks.steps(d.task_id()).unwrap().len(), 2);
    }

    #[test]
    fn an_unanswered_chip_is_sticky_and_a_noop_turn_counts_toward_the_cap() {
        let (mut d, tasks, _) = harness(vec![]);
        d.approve(1).unwrap();
        let risky = click("Delete account", AgentConfidence::High);
        assert!(matches!(d.classify(&risky, 2).unwrap(), Disposition::NeedsConfirm { .. }));
        // Claude re-calls with something else while the chip is up: still the chip.
        let harmless = click("Next", AgentConfidence::High);
        assert!(matches!(d.classify(&harmless, 3).unwrap(), Disposition::NeedsConfirm { .. }));
        assert_eq!(d.step(), 0, "nothing executed behind the chip");
        d.confirm(false, 4).unwrap();
        // Now a no-op turn: audited + counted.
        let noop = ActionInstruction {
            status: AgentStatus::Continue,
            reasoning: "just looking".into(),
            action: None,
            step_summary: None,
            confidence: AgentConfidence::High,
        };
        assert_eq!(d.classify(&noop, 5).unwrap(), Disposition::Noop);
        assert_eq!(d.step(), 2, "skip + noop are both steps");
        assert_eq!(tasks.steps(d.task_id()).unwrap().len(), 2);
    }

    #[test]
    fn approve_is_idempotent() {
        let (mut d, _, _) = harness(vec![]);
        d.approve(1).unwrap();
        d.approve(2).unwrap();
        assert_eq!(d.state(), TaskState::Running);
    }

    #[test]
    fn low_confidence_asks_even_for_a_harmless_label() {
        let (mut d, _, _) = harness(vec![]);
        d.approve(1).unwrap();
        let i = click("Next", AgentConfidence::Low);
        assert!(matches!(d.classify(&i, 2).unwrap(), Disposition::NeedsConfirm { .. }));
    }

    #[test]
    fn excluded_window_pauses_and_notifies_then_resume_continues() {
        let (mut d, tasks, _) = harness(vec![
            Err(ActionError::Excluded("1Password".into())),
            ok("clicked Next"),
        ]);
        d.approve(1).unwrap();
        let i = click("Next", AgentConfidence::High);
        let Disposition::Execute(a) = d.classify(&i, 2).unwrap() else { panic!() };
        assert!(matches!(d.execute(&i, &a, 2).unwrap(), Err(ActionError::Excluded(_))));
        assert_eq!(d.state(), TaskState::Paused);
        assert_eq!(d.pause(), Some(&PauseReason::Excluded { label: "1Password".into() }));
        assert_eq!(tasks.get_task(d.task_id()).unwrap().status, TaskState::Paused);
        d.resume(3).unwrap();
        let Disposition::Execute(a) = d.classify(&i, 4).unwrap() else { panic!() };
        d.execute(&i, &a, 4).unwrap().unwrap();
        assert_eq!(d.state(), TaskState::Running);
    }

    #[test]
    fn hard_stop_sets_the_executor_flag_first_and_is_final() {
        let (mut d, tasks, stop) = harness(vec![ok("x")]);
        d.approve(1).unwrap();
        d.hard_stop(2).unwrap();
        assert!(stop.load(Ordering::SeqCst));
        assert_eq!(d.state(), TaskState::Cancelled);
        assert_eq!(tasks.get_task(d.task_id()).unwrap().status, TaskState::Cancelled);
        let steps = tasks.steps(d.task_id()).unwrap();
        assert_eq!(steps[0].action_type.as_deref(), Some("hard_stop"));
        // Anything after is a no-op / Finished.
        assert!(d.hard_stop(3).is_ok());
        let i = click("Next", AgentConfidence::High);
        assert_eq!(d.classify(&i, 4).unwrap(), Disposition::Finished(StopReason::HardStop));
    }

    #[test]
    fn stop_flag_set_elsewhere_ends_the_loop_at_the_next_classify() {
        let (mut d, _, stop) = harness(vec![]);
        d.approve(1).unwrap();
        stop.store(true, Ordering::SeqCst);
        let i = click("Next", AgentConfidence::High);
        assert_eq!(d.classify(&i, 2).unwrap(), Disposition::Finished(StopReason::HardStop));
        assert_eq!(d.state(), TaskState::Cancelled);
    }

    #[test]
    fn fail_internal_marks_failed_without_a_user_stop() {
        let (mut d, tasks, stop) = harness(vec![]);
        d.approve(1).unwrap();
        d.fail_internal("the action crashed inside Aperture", 2);
        assert_eq!(d.state(), TaskState::Failed);
        assert!(!stop.load(Ordering::SeqCst), "an internal failure is not a user hard stop");
        assert!(matches!(d.stop_reason(), Some(StopReason::CannotProceed(_))));
        assert_eq!(tasks.get_task(d.task_id()).unwrap().status, TaskState::Failed);
        let steps = tasks.steps(d.task_id()).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].action_type.as_deref(), Some("internal_error"));
        assert_eq!(steps[0].result, Some(StepResult::Failure));
        // Terminal already ⇒ a second call is a no-op, no extra rows.
        d.fail_internal("again", 3);
        assert_eq!(tasks.steps(d.task_id()).unwrap().len(), 1);
    }

    #[test]
    fn three_failures_fail_the_task() {
        let (mut d, tasks, _) = harness(vec![
            Err(ActionError::ElementNotFound("a".into())),
            Err(ActionError::ElementNotFound("b".into())),
            Err(ActionError::ElementNotFound("c".into())),
        ]);
        d.approve(1).unwrap();
        let i = click("Next", AgentConfidence::High);
        for t in 2..5 {
            let Disposition::Execute(a) = d.classify(&i, t).unwrap() else { panic!() };
            let _ = d.execute(&i, &a, t).unwrap();
        }
        assert_eq!(d.state(), TaskState::Failed);
        assert_eq!(d.stop_reason(), Some(&StopReason::ErrorThreshold));
        assert_eq!(tasks.steps(d.task_id()).unwrap().len(), 3);
    }

    #[test]
    fn clarification_pauses_and_the_answer_rides_into_the_summary() {
        let (mut d, _, _) = harness(vec![]);
        d.approve(1).unwrap();
        let q = ActionInstruction {
            status: AgentStatus::NeedClarification,
            reasoning: "which account?".into(),
            action: None,
            step_summary: Some("opened settings".into()),
            confidence: AgentConfidence::High,
        };
        assert_eq!(
            d.classify(&q, 2).unwrap(),
            Disposition::Clarify { question: "which account?".into() }
        );
        assert_eq!(d.state(), TaskState::Paused);
        d.answer("the work one", 3).unwrap();
        assert_eq!(d.state(), TaskState::Running);
        assert_eq!(d.prior_summary(), Some("opened settings\nUser answered: the work one"));
    }

    #[test]
    fn deny_before_start_cancels() {
        let (mut d, tasks, _) = harness(vec![]);
        d.deny(1).unwrap();
        assert_eq!(d.state(), TaskState::Cancelled);
        assert_eq!(tasks.get_task(d.task_id()).unwrap().status, TaskState::Cancelled);
    }

    #[test]
    fn opened_windows_are_remembered_for_undo() {
        let w = WindowInfo { hwnd: 42, title: "Calculator".into(), process: Some("calc.exe".into()), window_class: None };
        let (mut d, _, _) = harness(vec![Ok(ActionOutcome {
            description: "launched".into(),
            focused_element: None,
            new_windows: vec![w.clone()],
        })]);
        d.approve(1).unwrap();
        let i = ActionInstruction {
            status: AgentStatus::Continue,
            reasoning: "open calc".into(),
            action: Some(AgentAction {
                action_type: ActionType::Launch,
                target: Some("Calculator".into()),
                value: None,
                direction: None,
                amount: None,
                coords: None,
            }),
            step_summary: None,
            confidence: AgentConfidence::High,
        };
        let Disposition::Execute(a) = d.classify(&i, 2).unwrap() else { panic!() };
        d.execute(&i, &a, 2).unwrap().unwrap();
        assert_eq!(d.undoable_windows(&[w.clone()]).len(), 1);
        assert!(d.undoable_windows(&[]).is_empty(), "closed already ⇒ nothing to undo");
        assert_eq!(d.log()[0].reversibility, "reversible");
    }
}
