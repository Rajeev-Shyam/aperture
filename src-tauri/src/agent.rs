//! The v2 agent runtime (Doc 22) — the shell's I/O around `agent-loop`'s
//! pure driver. One task at a time (Doc 22 §14).
//!
//! Shape (v2 kickoff §1): the loop rides the EXISTING MCP gate. Claude Desktop
//! calls `aperture_agent_start` / `aperture_agent_step` (see `mcp_bridge`);
//! each step call carries Claude's instruction for the last screen in and
//! returns the next screen out. Inside one call this module:
//!
//! 1. waits (bounded) for the user's decisions — task approval (decision #48,
//!    once per task), the confirmation chip (decision #47), a clarification
//!    answer, or a resume after an exclusion/elevation pause (#49/#50) — so
//!    Claude makes ONE call per turn instead of polling;
//! 2. executes the action on a blocking thread (UIA/SendInput are COM + sleeps)
//!    with the driver moved OUT of the runtime mutex, so the hard stop (a flag
//!    the executor checks before every action, plus `agent_hard_stop`) is
//!    never queued behind an in-flight action;
//! 3. observes: `CaptureSubsystem::observe_now` (same exclusion gate as a
//!    scheduled sample) → `screen_serializer::observe_frame` (OCR with boxes →
//!    image redaction → 768 px JPEG) → `build_step_payload` (text redaction,
//!    payload hash) → **audit row BEFORE release, fail-closed** (doc 13 §3,
//!    identical to `get_context`) → text + image content back over the pipe.
//!
//! What leaves the machine per step (Doc 22 §8, stated on the approval card):
//! the redacted screenshot, the redacted OCR text, the task text, window
//! titles/app names, and Claude's own rolling summary. Nothing else.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use aperture_action_executor::{
    list_open_windows, ActionExecutor, ExclusionProbe, UiaExecutor, WindowInfo,
};
use aperture_agent_loop::{AgentDriver, Disposition, LoopConfig, PauseReason, StopReason};
use aperture_capture::exclusion::ExclusionList;
use aperture_capture::CaptureError;
use aperture_contracts::agent::{ActionError, ActionInstruction, TaskState};
use aperture_privacy::audit_log::{AuditLog, AuditSink, CloudSendRecord};
use aperture_privacy::redaction::Redactor;
use aperture_screen_serializer::{
    build_step_payload, observe_frame, FocusedWindow, RawObservation,
};
use aperture_task_manager::{Task, TaskManager};
use aperture_vision_ocr::OcrEngine;

use crate::app_state::AppState;
use crate::{commands, events};

/// How long one `aperture_agent_step` call waits for the user before
/// returning a "still waiting — call again" notice. `aperture-mcp`'s pipe
/// round-trip times out at 180 s; this leaves headroom.
const USER_WAIT: Duration = Duration::from_secs(110);
/// Doc 22 §2 "wait for screen to settle" — the executor's hint is the source
/// of truth; this is the floor applied after a skip/noop.
const SETTLE_FLOOR: Duration = Duration::from_millis(300);
/// Open-window list cap in the payload (metadata only).
const OPEN_WINDOWS_MAX: usize = 20;

/// One exclusion-list reading for the executor (decision #49): the same shared
/// handle capture gates frames with, so a rule added from a bubble's
/// "Stop capturing X" protects the agent's hands on the next action too.
pub struct ExclusionListProbe(pub ExclusionList);

impl ExclusionProbe for ExclusionListProbe {
    fn excluded_label(
        &self,
        process: Option<&str>,
        window_class: Option<&str>,
        title: Option<&str>,
    ) -> Option<String> {
        match self.0.is_excluded(process, window_class, title, None) {
            aperture_capture::exclusion::ExclusionVerdict::Excluded { label, .. } => Some(label),
            aperture_capture::exclusion::ExclusionVerdict::Allowed => None,
        }
    }
}

/// A window the task opened that is still open — the honest undo offer
/// (decision #54).
#[derive(Debug, Clone, Serialize)]
pub struct WindowRef {
    pub hwnd: isize,
    pub title: String,
}

/// What the overlay renders (decision #53: persistent status bar + live log)
/// — broadcast as the `agent_task` event on every change.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub description: String,
    /// `"claude"` (Claude asked) or `"user"` (typed into Aperture).
    pub source: String,
    pub state: String,
    pub step: u32,
    pub step_cap: u32,
    pub pause: Option<PauseReason>,
    pub log: Vec<aperture_agent_loop::StepLogEntry>,
    pub outcome: Option<String>,
    pub stop_reason: Option<String>,
    pub undoable_windows: Vec<WindowRef>,
    /// An action is executing right now (the driver is out of the mutex).
    pub in_flight: bool,
}

/// One step's reply over the pipe: text (the payload JSON minus the image,
/// plus a short instruction) and the redacted screenshot as an image block.
pub struct StepReply {
    pub text: String,
    pub image_jpeg: Option<Vec<u8>>,
    pub is_error: bool,
}

/// The runtime. `driver` is `None` between tasks and while an action is in
/// flight (it is moved onto the blocking thread and put back afterwards).
pub struct AgentRuntime {
    tasks: Arc<TaskManager>,
    ocr: Option<Arc<dyn OcrEngine>>,
    exclusions: ExclusionList,
    driver: Option<AgentDriver>,
    stop: Arc<AtomicBool>,
    in_flight: bool,
    source: &'static str,
    /// The last rendered view — served while the driver is in flight.
    snapshot: Option<TaskView>,
    /// Set by the confirmation chip's "Approve": the instruction to execute
    /// on the waiting step's next turn.
    approved_action: Option<ActionInstruction>,
}

impl AgentRuntime {
    pub fn new(tasks: Arc<TaskManager>, ocr: Option<Arc<dyn OcrEngine>>, exclusions: ExclusionList) -> Self {
        Self {
            tasks,
            ocr,
            exclusions,
            driver: None,
            stop: Arc::new(AtomicBool::new(false)),
            in_flight: false,
            source: "user",
            snapshot: None,
            approved_action: None,
        }
    }

    pub fn tasks(&self) -> &Arc<TaskManager> {
        &self.tasks
    }

    /// Is `id` the current task AND still live?
    pub fn is_current_live(&self, id: uuid::Uuid) -> bool {
        self.in_flight
            || self
                .driver
                .as_ref()
                .map(|d| d.task_id() == id && !d.is_terminal())
                .unwrap_or(false)
    }

    /// Drop a finished task from the overlay (the durable rows stay).
    pub fn dismiss_if_terminal(&mut self) -> Result<(), String> {
        if self.has_live_task() {
            return Err("the task is still running — stop it first".into());
        }
        self.driver = None;
        self.snapshot = None;
        self.approved_action = None;
        Ok(())
    }

    /// The current view, or `None` when no task exists.
    pub fn view(&self) -> Option<TaskView> {
        match &self.driver {
            Some(d) => Some(render_view(d, self.source, self.in_flight)),
            None => self.snapshot.clone(),
        }
    }

    fn has_live_task(&self) -> bool {
        self.in_flight
            || self
                .driver
                .as_ref()
                .map(|d| !d.is_terminal())
                .unwrap_or(false)
    }

    /// Create a task (Doc 22 §9.1: stored immediately). `pre_approved` is the
    /// user-typed path — they initiated it in Aperture, which IS the approval.
    fn create(
        &mut self,
        description: &str,
        pre_approved: bool,
        config: LoopConfig,
        now_ms: i64,
    ) -> Result<Task, String> {
        if self.has_live_task() {
            return Err("an agent task is already running — stop it first".into());
        }
        let task = self.tasks.create_task(description, now_ms).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let probe: Arc<dyn ExclusionProbe> = Arc::new(ExclusionListProbe(self.exclusions.clone()));
        let executor: Arc<dyn ActionExecutor> =
            Arc::new(UiaExecutor::new(probe, Arc::clone(&stop)));
        let mut driver =
            AgentDriver::new(task.clone(), executor, Arc::clone(&self.tasks), Arc::clone(&stop), config);
        if pre_approved {
            driver.approve(now_ms).map_err(|e| e.to_string())?;
        }
        self.stop = stop;
        self.driver = Some(driver);
        self.source = if pre_approved { "user" } else { "claude" };
        self.in_flight = false;
        self.approved_action = None;
        self.snapshot = self.view();
        Ok(task)
    }
}

fn render_view(d: &AgentDriver, source: &str, in_flight: bool) -> TaskView {
    let open = list_open_windows();
    TaskView {
        task_id: d.task_id().to_string(),
        description: d.task().description.clone(),
        source: source.to_string(),
        state: d.state().as_str().to_string(),
        step: d.step(),
        step_cap: d.step_cap(),
        pause: d.pause().cloned(),
        log: d.log().to_vec(),
        outcome: d.task().outcome_summary.clone(),
        stop_reason: d.stop_reason().map(StopReason::describe),
        undoable_windows: d
            .undoable_windows(&open)
            .into_iter()
            .map(|w| WindowRef { hwnd: w.hwnd, title: w.title.clone() })
            .collect(),
        in_flight,
    }
}

/// Read the `agent` settings section (seeded in settings.default.json).
pub fn loop_config(db: &aperture_db::Db) -> LoopConfig {
    let mut cfg = LoopConfig::default();
    let Some(raw) = db.get_setting("agent").ok().flatten() else { return cfg };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { return cfg };
    if let Some(n) = v.get("step_cap").and_then(|x| x.as_u64()) {
        cfg.step_cap = (n as u32).clamp(1, 500);
    }
    if let Some(n) = v.get("error_threshold").and_then(|x| x.as_u64()) {
        cfg.error_threshold = (n as u32).clamp(1, 20);
    }
    if let Some(b) = v.get("confirm_low_confidence").and_then(|x| x.as_bool()) {
        cfg.confirm_low_confidence = b;
    }
    cfg
}

/// Broadcast the current view to every overlay window.
pub async fn emit_view(app: &tauri::AppHandle, state: &AppState) {
    let view = state.agent.lock().await.view();
    let _ = events::emit_agent_task(app, view.as_ref());
}

fn notify(state: &AppState) {
    state.agent_notify.notify_waiters();
}

// ---------------------------------------------------------------------------
// Entry points used by the Tauri commands (user side)
// ---------------------------------------------------------------------------

/// The user typed a task into Aperture (Doc 22 §9.1) — created AND approved
/// in one go (locked decision 4: the user initiated it). Claude adopts it
/// with `aperture_agent_start` (no `task`).
pub async fn user_start_task(app: &tauri::AppHandle, state: &AppState, description: &str) -> Result<TaskView, String> {
    let description = description.trim();
    if description.is_empty() {
        return Err("describe the task first".into());
    }
    let cfg = loop_config(&state.db);
    let now = crate::pipeline::epoch_ms();
    let view = {
        let mut rt = state.agent.lock().await;
        rt.create(description, true, cfg, now)?;
        rt.view()
    };
    notify(state);
    let _ = events::emit_agent_task(app, view.as_ref());
    view.ok_or_else(|| "task view unavailable".into())
}

/// The user pressed a button on the agent surface. `decision` ∈
/// approve | deny | confirm | skip | resume | stop.
pub async fn user_decide(app: &tauri::AppHandle, state: &AppState, task_id: &str, decision: &str) -> Result<TaskView, String> {
    let id = uuid::Uuid::parse_str(task_id).map_err(|e| e.to_string())?;
    let now = crate::pipeline::epoch_ms();
    let view = {
        let mut rt = state.agent.lock().await;
        if decision == "stop" {
            // Flag first: an action that has not started is refused even if
            // the driver is on the blocking thread right now.
            rt.stop.store(true, Ordering::SeqCst);
        }
        let Some(d) = rt.driver.as_mut() else {
            if rt.in_flight && decision == "stop" {
                return rt.view().ok_or_else(|| "no task".into());
            }
            return Err("no agent task".into());
        };
        if d.task_id() != id {
            return Err("that task is no longer current".into());
        }
        let result = match decision {
            "approve" => d.approve(now).map(|_| None),
            "deny" => d.deny(now).map(|_| None),
            "confirm" => d.confirm(true, now),
            "skip" => d.confirm(false, now),
            "resume" => d.resume(now).map(|_| None),
            "stop" => d.hard_stop(now).map(|_| None),
            other => return Err(format!("unknown decision {other}")),
        };
        match result {
            Ok(Some(instr)) => rt.approved_action = Some(instr),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        rt.snapshot = rt.view();
        rt.view()
    };
    notify(state);
    let _ = events::emit_agent_task(app, view.as_ref());
    view.ok_or_else(|| "no task".into())
}

/// The user answered Claude's question (Doc 22 §5 `need_clarification`).
pub async fn user_answer(app: &tauri::AppHandle, state: &AppState, task_id: &str, answer: &str) -> Result<TaskView, String> {
    let id = uuid::Uuid::parse_str(task_id).map_err(|e| e.to_string())?;
    let now = crate::pipeline::epoch_ms();
    let view = {
        let mut rt = state.agent.lock().await;
        let Some(d) = rt.driver.as_mut() else { return Err("no agent task".into()) };
        if d.task_id() != id {
            return Err("that task is no longer current".into());
        }
        d.answer(answer, now).map_err(|e| e.to_string())?;
        rt.snapshot = rt.view();
        rt.view()
    };
    notify(state);
    let _ = events::emit_agent_task(app, view.as_ref());
    view.ok_or_else(|| "no task".into())
}

/// Decision #54: close the windows this task opened that are still open, by
/// the same simulated-UI means the task used (switch + Alt+F4). Only ever
/// the recorded windows; returns how many were closed.
pub async fn user_undo_close_windows(app: &tauri::AppHandle, state: &AppState, task_id: &str) -> Result<usize, String> {
    let id = uuid::Uuid::parse_str(task_id).map_err(|e| e.to_string())?;
    let targets: Vec<WindowInfo> = {
        let rt = state.agent.lock().await;
        let Some(d) = rt.driver.as_ref() else { return Err("no agent task".into()) };
        if d.task_id() != id {
            return Err("that task is no longer current".into());
        }
        let open = list_open_windows();
        d.undoable_windows(&open).into_iter().cloned().collect()
    };
    let probe: Arc<dyn ExclusionProbe> = Arc::new(ExclusionListProbe(state.exclusions.clone()));
    let closed = tokio::task::spawn_blocking(move || {
        aperture_action_executor::close_windows(&targets, probe.as_ref())
    })
    .await
    .map_err(|e| e.to_string())?;
    emit_view(app, state).await;
    Ok(closed)
}

// ---------------------------------------------------------------------------
// Entry points used by the MCP bridge (Claude side)
// ---------------------------------------------------------------------------

/// `aperture_agent_start`. With `task`: create it and show the approval card
/// (decision #48). Without: adopt the user-typed task if one is waiting.
pub async fn mcp_start(app: &tauri::AppHandle, state: &AppState, task: Option<&str>) -> Result<(String, bool), String> {
    let now = crate::pipeline::epoch_ms();
    let cfg = loop_config(&state.db);
    let out = {
        let mut rt = state.agent.lock().await;
        match task.map(str::trim).filter(|t| !t.is_empty()) {
            Some(desc) => {
                let t = rt.create(desc, false, cfg, now)?;
                (t.id.to_string(), false)
            }
            None => match rt.driver.as_ref() {
                Some(d) if !d.is_terminal() => (d.task_id().to_string(), d.is_approved()),
                _ => {
                    return Err(
                        "no task is waiting in Aperture — pass the user's task as `task`, or ask \
                         them to type it into Aperture first"
                            .into(),
                    )
                }
            },
        }
    };
    notify(state);
    emit_view(app, state).await;
    Ok(out)
}

/// Wait until `pred` holds for the current driver, waking on every user
/// decision, for at most [`USER_WAIT`]. Returns whether it holds.
async fn wait_for(state: &AppState, pred: impl Fn(&AgentRuntime) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + USER_WAIT;
    loop {
        if pred(&*state.agent.lock().await) {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let notified = state.agent_notify.notified();
        let _ = tokio::time::timeout(deadline - now, notified).await;
    }
}

fn driver_matches(rt: &AgentRuntime, id: uuid::Uuid) -> bool {
    rt.driver.as_ref().map(|d| d.task_id() == id).unwrap_or(false)
}

/// `aperture_agent_step` — see the module doc for the phases.
pub async fn mcp_step(
    app: &tauri::AppHandle,
    state: &AppState,
    task_id: &str,
    instruction: Option<serde_json::Value>,
) -> Result<StepReply, String> {
    let id = uuid::Uuid::parse_str(task_id).map_err(|_| "task_id is not a UUID".to_string())?;

    // A. the task must exist, be ours, and be approved.
    {
        let rt = state.agent.lock().await;
        if rt.in_flight {
            return Ok(notice("another step is still executing — call again in a moment."));
        }
        if !driver_matches(&rt, id) {
            return Err("unknown or superseded task_id — call aperture_agent_start".into());
        }
    }
    if !wait_for(state, |rt| {
        rt.driver
            .as_ref()
            .map(|d| d.is_approved() || d.is_terminal())
            .unwrap_or(true)
    })
    .await
    {
        return Ok(notice(
            "the user has not approved this task yet — it is waiting on their screen. Call again.",
        ));
    }
    if let Some(reply) = terminal_reply(state, id).await {
        return Ok(reply);
    }

    // B. Claude's instruction for the last screen.
    let now = crate::pipeline::epoch_ms();
    let pending_approved = state.agent.lock().await.approved_action.take();
    let to_execute: Option<(ActionInstruction, aperture_contracts::agent::AgentAction)> =
        if let Some(instr) = pending_approved {
            instr.action.clone().map(|a| (instr, a))
        } else if let Some(raw) = instruction {
            let instr: ActionInstruction = match serde_json::from_value(raw) {
                Ok(i) => i,
                Err(e) => {
                    // Q-V2-07 (provisional): a malformed instruction is a failed
                    // step — audited, counted toward the threshold, echoed back.
                    let mut rt = state.agent.lock().await;
                    if let Some(d) = rt.driver.as_mut() {
                        d.note_malformed(&e.to_string(), now);
                    }
                    drop(rt);
                    emit_view(app, state).await;
                    if let Some(reply) = terminal_reply(state, id).await {
                        return Ok(reply);
                    }
                    return Ok(StepReply {
                        text: format!(
                            "instruction did not parse ({e}). Send the JSON object described by the \
                             tool; this counted as a failed step."
                        ),
                        image_jpeg: None,
                        is_error: true,
                    });
                }
            };
            let disposition = {
                let mut rt = state.agent.lock().await;
                let Some(d) = rt.driver.as_mut() else { return Err("task vanished".into()) };
                d.classify(&instr, now).map_err(|e| e.to_string())?
            };
            emit_view(app, state).await;
            match disposition {
                Disposition::Finished(_) => {
                    finish_side_effects(app, state).await;
                    return Ok(terminal_reply(state, id).await.unwrap_or_else(|| notice("task ended")));
                }
                Disposition::Clarify { question } => {
                    notify(state);
                    let answered =
                        wait_for(state, |rt| rt.driver.as_ref().map(|d| d.state() != TaskState::Paused).unwrap_or(true)).await;
                    if !answered {
                        return Ok(notice(format!(
                            "paused: waiting for the user to answer \"{question}\". Call again (no instruction) to continue once they have."
                        )));
                    }
                    if let Some(reply) = terminal_reply(state, id).await {
                        return Ok(reply);
                    }
                    None
                }
                Disposition::NeedsConfirm { reason } => {
                    notify(state);
                    let decided = wait_for(state, |rt| {
                        rt.approved_action.is_some()
                            || rt
                                .driver
                                .as_ref()
                                .map(|d| !matches!(d.pause(), Some(PauseReason::Confirm { .. })))
                                .unwrap_or(true)
                    })
                    .await;
                    if !decided {
                        return Ok(notice(format!(
                            "paused: the action {reason} — waiting for the user to approve or skip it. Call again with the SAME instruction once they have."
                        )));
                    }
                    if let Some(reply) = terminal_reply(state, id).await {
                        return Ok(reply);
                    }
                    let approved = state.agent.lock().await.approved_action.take();
                    approved.and_then(|i| i.action.clone().map(|a| (i, a)))
                }
                Disposition::Execute(a) => Some((instr, a)),
                Disposition::Noop => None,
            }
        } else {
            None
        };

    // Execute on a blocking thread with the driver moved out of the mutex.
    let mut settle = SETTLE_FLOOR;
    if let Some((instr, action)) = to_execute {
        let driver = {
            let mut rt = state.agent.lock().await;
            let d = rt.driver.take().ok_or_else(|| "task vanished".to_string())?;
            rt.in_flight = true;
            rt.snapshot = Some(render_view(&d, rt.source, true));
            d
        };
        emit_view(app, state).await;
        let (driver, outcome) = tokio::task::spawn_blocking(move || {
            let mut d = driver;
            let r = d.execute(&instr, &action, crate::pipeline::epoch_ms());
            (d, r)
        })
        .await
        .map_err(|e| e.to_string())?;
        settle = driver.settle_hint().max(SETTLE_FLOOR);
        {
            let mut rt = state.agent.lock().await;
            rt.driver = Some(driver);
            rt.in_flight = false;
            rt.snapshot = rt.view();
        }
        notify(state);
        emit_view(app, state).await;
        match outcome {
            Err(e) => return Err(e.to_string()),
            Ok(Err(ActionError::Excluded(_))) | Ok(Err(ActionError::Elevated(_))) => {
                // Decision #49/#50: paused + notified; wait for Resume/Stop.
                if !wait_for_resume(state, id).await {
                    return Ok(notice(
                        "paused: the window in front is excluded or elevated — Aperture is asking the user. Call again (no instruction) after they resume.",
                    ));
                }
                if let Some(reply) = terminal_reply(state, id).await {
                    return Ok(reply);
                }
            }
            Ok(Err(ActionError::Stopped)) => {
                finish_side_effects(app, state).await;
                return Ok(terminal_reply(state, id).await.unwrap_or_else(|| notice("stopped")));
            }
            Ok(_) => {
                if let Some(reply) = terminal_reply(state, id).await {
                    finish_side_effects(app, state).await;
                    return Ok(reply);
                }
            }
        }
    }
    tokio::time::sleep(settle).await;

    // C. observe — the only place user data leaves, audited before release.
    observe_and_release(app, state, id).await
}

async fn wait_for_resume(state: &AppState, id: uuid::Uuid) -> bool {
    wait_for(state, |rt| {
        rt.driver
            .as_ref()
            .map(|d| d.task_id() != id || d.state() != TaskState::Paused)
            .unwrap_or(true)
    })
    .await
}

/// The task-ended reply, if the task is terminal.
async fn terminal_reply(state: &AppState, id: uuid::Uuid) -> Option<StepReply> {
    let rt = state.agent.lock().await;
    let d = rt.driver.as_ref()?;
    if d.task_id() != id || !d.is_terminal() {
        return None;
    }
    let reason = d.stop_reason().map(StopReason::describe).unwrap_or_else(|| "ended".into());
    let outcome = d.task().outcome_summary.clone().unwrap_or_default();
    Some(StepReply {
        text: format!(
            "task {} — {} ({} steps). Outcome: {outcome}. No further actions will be taken.",
            d.state().as_str(),
            reason,
            d.step()
        ),
        image_jpeg: None,
        is_error: false,
    })
}

/// Terminal bookkeeping the user should see: the view (the surface renders
/// the outcome + undo offer) and, for a hard stop, nothing else — the audit
/// row was written by the driver.
async fn finish_side_effects(app: &tauri::AppHandle, state: &AppState) {
    notify(state);
    emit_view(app, state).await;
}

fn notice(text: impl Into<String>) -> StepReply {
    StepReply { text: text.into(), image_jpeg: None, is_error: false }
}

/// Observe the screen, build + hash the payload, audit, release.
async fn observe_and_release(app: &tauri::AppHandle, state: &AppState, id: uuid::Uuid) -> Result<StepReply, String> {
    let (task_text, step_number, last_action, prior) = {
        let rt = state.agent.lock().await;
        let d = rt.driver.as_ref().ok_or_else(|| "task vanished".to_string())?;
        (
            d.task().description.clone(),
            d.step() + 1,
            d.last_action().cloned(),
            d.prior_summary().map(str::to_string),
        )
    };
    let ocr = state.agent.lock().await.ocr.clone();
    let capture = Arc::clone(&state.capture);
    let redactor = Redactor::new(&commands::read_user_redaction_terms(&state.db))
        .map_err(|e| e.to_string())?;

    let observed = tokio::task::spawn_blocking(move || -> Result<RawObservation, CaptureError> {
        let obs = capture.observe_now()?;
        let identity = obs.identity.clone();
        let focused = FocusedWindow {
            app: identity.app.clone().or(identity.process.clone()).unwrap_or_default(),
            title: identity.window_title.clone().unwrap_or_default(),
            url: obs.url.clone(),
        };
        let mut open: Vec<String> = list_open_windows()
            .into_iter()
            .map(|w| w.process.map(|p| format!("{} — {}", p, w.title)).unwrap_or(w.title))
            .collect();
        open.dedup();
        open.truncate(OPEN_WINDOWS_MAX);
        let (ocr_text, screenshot) = match ocr.as_ref() {
            Some(engine) => {
                match observe_frame(obs.frame.bgra(), obs.frame.width, obs.frame.height, engine.as_ref(), &redactor) {
                    Ok(o) => (o.ocr_text, Some(o.screenshot)),
                    Err(e) => {
                        tracing::warn!(error = %e, "agent observe: OCR/redaction failed — text-only observation");
                        (String::new(), None)
                    }
                }
            }
            None => (String::new(), None),
        };
        drop(obs); // the raw frame dies here (doc 05 §2)
        Ok(RawObservation { ocr_text, focused_window: focused, open_windows: open, screenshot })
    })
    .await
    .map_err(|e| e.to_string())?;

    let observation = match observed {
        Ok(o) => o,
        Err(CaptureError::Excluded(label)) => {
            {
                let mut rt = state.agent.lock().await;
                if let Some(d) = rt.driver.as_mut() {
                    let _ = d.pause_for(PauseReason::Excluded { label: label.clone() }, crate::pipeline::epoch_ms());
                }
                rt.snapshot = rt.view();
            }
            notify(state);
            emit_view(app, state).await;
            return Ok(notice(format!(
                "paused: the window in front (\"{label}\") is on the user's exclusion list — nothing was observed. Aperture is asking the user; call again (no instruction) after they resume."
            )));
        }
        Err(e) => {
            return Ok(StepReply { text: format!("cannot observe the screen: {e}"), image_jpeg: None, is_error: true })
        }
    };

    let redactor = Redactor::new(&commands::read_user_redaction_terms(&state.db)).map_err(|e| e.to_string())?;
    let (payload, hash) = build_step_payload(&task_text, step_number, observation, last_action, prior, &redactor);

    // Wire = the JSON text Claude reads + the image block; both are covered by
    // the payload hash (screenshot_b64 is inside the hashed payload).
    let image = payload
        .screenshot_b64
        .as_deref()
        .and_then(|b| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(b).ok()
        });
    let mut text_payload = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
    if image.is_some() {
        text_payload["screenshot_b64"] = serde_json::Value::String("<attached as image>".into());
    }
    let text = serde_json::to_string(&text_payload).map_err(|e| e.to_string())?;
    let byte_count = text.len() as u64 + image.as_ref().map(|i| i.len() as u64).unwrap_or(0);

    // Audit BEFORE release, fail-closed (doc 13 §3; same as get_context).
    let audit = AuditLog::new(Arc::clone(&state.db));
    if let Err(e) = audit.record_cloud_send(CloudSendRecord {
        payload_id: uuid::Uuid::new_v4(),
        wire_sha256: hash.clone(),
        transport: aperture_contracts::TransportTarget::ClaudeDesktopMcp,
        byte_count,
        ts: crate::pipeline::epoch_ms(),
    }) {
        let _ = events::emit_audit_alert(
            app,
            &format!("Agent step audit write failed ({e}). The screen was NOT sent — nothing left this machine."),
        );
        return Err(format!("cloud_send audit write failed — screen NOT released: {e}"));
    }
    {
        let mut rt = state.agent.lock().await;
        if let Some(d) = rt.driver.as_mut() {
            d.observed(hash);
        }
        rt.snapshot = rt.view();
    }
    emit_view(app, state).await;
    tracing::info!(task = %id, step = step_number, bytes = byte_count, "agent step payload released to Claude Desktop (MCP)");
    Ok(StepReply {
        text: format!(
            "{text}\n\nReply by calling aperture_agent_step again with task_id and your `instruction` for THIS screen."
        ),
        image_jpeg: image,
        is_error: false,
    })
}

/// Build the runtime for `AppState`. The OCR engine is a second
/// `WindowsMediaOcr` instance (cheap; the capture sink owns its own).
pub fn build_runtime(db: Arc<aperture_db::Db>, exclusions: ExclusionList) -> AgentRuntime {
    let ocr: Option<Arc<dyn OcrEngine>> = match aperture_vision_ocr::windows_media_ocr::WindowsMediaOcr::new("en") {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::warn!(error = %e, "agent runtime: Windows OCR unavailable — observations will be text-less");
            None
        }
    };
    AgentRuntime::new(Arc::new(TaskManager::new(db)), ocr, exclusions)
}

/// Hook for the tray's "Stop agent task" item and the command: flag first,
/// then transition.
pub async fn hard_stop_current(app: &tauri::AppHandle, state: &AppState) -> Option<TaskView> {
    let id = {
        let rt = state.agent.lock().await;
        rt.stop.store(true, Ordering::SeqCst);
        rt.driver.as_ref().map(|d| d.task_id().to_string())
    };
    match id {
        Some(id) => user_decide(app, state, &id, "stop").await.ok(),
        None => None,
    }
}
