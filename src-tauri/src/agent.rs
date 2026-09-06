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
use aperture_capture::exclusion::{ExclusionList, ExclusionVerdict};
use aperture_capture::{CaptureError, CaptureSubsystem};
use aperture_contracts::agent::{ActionError, ActionInstruction, TaskState};
use aperture_privacy::audit_log::{AuditLog, AuditSink, CloudSendRecord};
use aperture_privacy::redaction::Redactor;
// Decision #42's transport cap — enforced HERE, on the exact wire bytes and
// before the cloud_send row (08-22 review); mcp_bridge only tripwires it.
use aperture_reasoning_gateway::transports::mcp::MCP_RESULT_MAX_BYTES;
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
/// One `mcp_step` call's TOTAL wall-clock budget. Several waits can run
/// back-to-back inside one call (approval → pause gate → confirm → resume);
/// unbounded, they stack past `aperture-mcp`'s 180 s pipe CALL_TIMEOUT and the
/// handler keeps running against a dead pipe — a decision consumed, a screen
/// released, and nobody listening (08-22 review). Every wait is therefore
/// clipped to one shared deadline; 150 s keeps the same headroom under the
/// pipe that USER_WAIT keeps per wait.
const CALL_BUDGET: Duration = Duration::from_secs(150);
/// Doc 22 §2 "wait for screen to settle" — the executor's hint is the source
/// of truth; this is the floor applied after a skip/noop.
const SETTLE_FLOOR: Duration = Duration::from_millis(300);
/// Open-window list cap in the payload (metadata only).
const OPEN_WINDOWS_MAX: usize = 20;
/// The one error every id-check in `mcp_step` answers with.
const SUPERSEDED: &str = "unknown or superseded task_id — call aperture_agent_start";
/// The probe's fail-closed label (decision #49): a browser is in front, the
/// list carries `url_pattern` rules, and no URL could be resolved — the agent
/// pauses rather than act on a page that might be one of them.
pub const BROWSER_PAGE_UNIDENTIFIED: &str = "browser page could not be identified";

/// One exclusion-list reading for the executor (decision #49): the same shared
/// handle capture gates frames with, so a rule added from a bubble's
/// "Stop capturing X" protects the agent's hands on the next action too.
/// For a BROWSER foreground it also resolves the live URL through capture's
/// own resolver (extension feed → UIA address bar → last-known) so
/// `url_pattern` rules reach the executor, and fails CLOSED when such rules
/// exist but no URL can be resolved (08-22 review; see [`probe_decision`]).
pub struct ExclusionListProbe {
    list: ExclusionList,
    capture: Arc<CaptureSubsystem>,
}

impl ExclusionListProbe {
    pub fn new(list: ExclusionList, capture: Arc<CaptureSubsystem>) -> Self {
        Self { list, capture }
    }
}

impl ExclusionProbe for ExclusionListProbe {
    fn excluded_label(&self, w: &WindowInfo) -> Option<String> {
        let process = w.process.as_deref();
        let is_browser = process.map(|p| self.capture.is_browser_process(p)).unwrap_or(false);
        let url = match process {
            Some(p) if is_browser => self.capture.resolve_url_for(w.hwnd, p),
            _ => None,
        };
        let verdict = self.list.is_excluded(
            process,
            w.window_class.as_deref(),
            Some(w.title.as_str()),
            url.as_deref(),
        );
        probe_decision(is_browser, self.list.has_url_rules(), url.as_deref(), verdict)
    }
}

/// The probe's decision, pure: a rule hit is always a hit; otherwise a browser
/// page whose URL is unknown while `url_pattern` rules exist fails closed
/// ([`BROWSER_PAGE_UNIDENTIFIED`]); everything else (non-browsers, browsers
/// with a resolved-and-allowed URL, lists without URL rules) is unchanged.
fn probe_decision(
    is_browser: bool,
    has_url_rules: bool,
    url: Option<&str>,
    verdict: ExclusionVerdict,
) -> Option<String> {
    match verdict {
        ExclusionVerdict::Excluded { label, .. } => Some(label),
        ExclusionVerdict::Allowed if is_browser && has_url_rules && url.is_none() => {
            Some(BROWSER_PAGE_UNIDENTIFIED.into())
        }
        ExclusionVerdict::Allowed => None,
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
    /// The user pressed Stop but the task is not terminal yet (the driver is
    /// mid-action on the blocking thread) — the bar acknowledges with
    /// "Stopping…" instead of a stale "running" (08-22 review). The hard stop
    /// is applied the moment the action returns.
    pub stopping: bool,
}

/// One step's reply over the pipe: text (the payload JSON minus the image,
/// plus a short instruction) and the redacted screenshot as its base64 string,
/// sent VERBATIM as the MCP image block's `data` — so the wire hash computed
/// over it in `observe_and_release` is the hash of what actually leaves.
pub struct StepReply {
    pub text: String,
    pub image_b64: Option<String>,
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
            Some(d) => Some(render_view(
                d,
                self.source,
                self.in_flight,
                self.stop.load(Ordering::SeqCst) && !d.is_terminal(),
            )),
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
        capture: Arc<CaptureSubsystem>,
    ) -> Result<Task, String> {
        if self.has_live_task() {
            return Err("an agent task is already running — stop it first".into());
        }
        let task = self.tasks.create_task(description, now_ms).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let probe: Arc<dyn ExclusionProbe> =
            Arc::new(ExclusionListProbe::new(self.exclusions.clone(), capture));
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

fn render_view(d: &AgentDriver, source: &str, in_flight: bool, stopping: bool) -> TaskView {
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
        stopping,
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
        rt.create(description, true, cfg, now, Arc::clone(&state.capture))?;
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
            // Only the CURRENT task's stop flag may be raised by `id` (08-22
            // review): a surface holding a stale id must not cancel the task
            // that replaced it. (The tray's no-id path in `hard_stop_current`
            // stays unconditional.)
            let current = stop_targets(
                rt.in_flight,
                rt.driver.as_ref().map(|d| d.task_id()),
                rt.snapshot.as_ref().map(|s| s.task_id.as_str()),
                id,
            );
            if !current {
                return Err(if rt.driver.is_none() && !rt.in_flight {
                    "no agent task".into()
                } else {
                    "that task is no longer current".into()
                });
            }
            // Flag first: an action that has not started is refused even if
            // the driver is on the blocking thread right now.
            rt.stop.store(true, Ordering::SeqCst);
        }
        let Some(d) = rt.driver.as_mut() else {
            if rt.in_flight && decision == "stop" {
                // The driver is on the blocking thread: the flag above is
                // applied the moment the action returns. The bar must say so
                // NOW, not keep claiming "running" (08-22 review) — stamp the
                // snapshot and broadcast it.
                if let Some(snap) = rt.snapshot.as_mut() {
                    snap.stopping = true;
                }
                let view = rt.view();
                drop(rt);
                notify(state);
                let _ = events::emit_agent_task(app, view.as_ref());
                return view.ok_or_else(|| "no task".into());
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
    let probe: Arc<dyn ExclusionProbe> = Arc::new(ExclusionListProbe::new(
        state.exclusions.clone(),
        Arc::clone(&state.capture),
    ));
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
                let t = rt.create(desc, false, cfg, now, Arc::clone(&state.capture))?;
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
/// decision, for at most [`USER_WAIT`] — clipped to `call_deadline`, the
/// call-wide [`CALL_BUDGET`] deadline, so stacked waits can never outlive the
/// pipe. Returns whether the predicate holds (a satisfied predicate wins even
/// with the budget already spent; an exhausted budget returns `false`, and
/// the caller answers with the same notice its timeout path uses).
async fn wait_for(
    state: &AppState,
    call_deadline: tokio::time::Instant,
    pred: impl Fn(&AgentRuntime) -> bool,
) -> bool {
    let deadline = (tokio::time::Instant::now() + USER_WAIT).min(call_deadline);
    loop {
        // tokio's documented `Notify` pattern (08-22 review): the `Notified`
        // future must exist AND be enabled BEFORE the predicate is checked —
        // `notify_waiters` wakes only already-registered waiters, so a
        // decision landing between the check and the await would otherwise
        // sleep out the whole deadline.
        let notified = state.agent_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if pred(&*state.agent.lock().await) {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let _ = tokio::time::timeout(deadline - now, notified).await;
    }
}

fn driver_matches(rt: &AgentRuntime, id: uuid::Uuid) -> bool {
    rt.driver.as_ref().map(|d| d.task_id() == id).unwrap_or(false)
}

/// May a Stop carrying `id` raise the CURRENT stop flag (08-22 review)? Only
/// when `id` is the driver in the mutex, or the driver out on the blocking
/// thread — whose identity lives in the snapshot while it is out.
fn stop_targets(
    in_flight: bool,
    driver_id: Option<uuid::Uuid>,
    in_flight_id: Option<&str>,
    id: uuid::Uuid,
) -> bool {
    driver_id == Some(id)
        || (in_flight && in_flight_id.map(|s| s == id.to_string()).unwrap_or(false))
}

/// Take the chip-approved instruction — only when the driver is still THIS
/// call's task (08-22 review: identity re-validated after every wait). A
/// superseding task's approval must never execute under an older call.
async fn take_approved_if_current(
    state: &AppState,
    id: uuid::Uuid,
) -> Result<Option<ActionInstruction>, String> {
    let mut rt = state.agent.lock().await;
    if !driver_matches(&rt, id) {
        return Err(SUPERSEDED.into());
    }
    Ok(rt.approved_action.take())
}

/// `aperture_agent_step` — see the module doc for the phases.
pub async fn mcp_step(
    app: &tauri::AppHandle,
    state: &AppState,
    task_id: &str,
    instruction: Option<serde_json::Value>,
) -> Result<StepReply, String> {
    let id = uuid::Uuid::parse_str(task_id).map_err(|_| "task_id is not a UUID".to_string())?;
    // ONE deadline for every wait in this call (08-22 review; see CALL_BUDGET).
    let call_deadline = tokio::time::Instant::now() + CALL_BUDGET;

    // A. the task must exist, be ours, and be approved.
    {
        let rt = state.agent.lock().await;
        if rt.in_flight {
            return Ok(notice("another step is still executing — call again in a moment."));
        }
        if !driver_matches(&rt, id) {
            return Err(SUPERSEDED.into());
        }
    }
    if !wait_for(state, call_deadline, |rt| {
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
    // The Resume gate is ENFORCED, not advisory (08-22 review): the task must
    // be Running and un-paused before Claude may instruct or observe — a step
    // call arriving while Paused (excluded/elevated/clarification) or while a
    // confirmation chip is unanswered waits here, then returns the pause
    // notice instead of acting.
    if !wait_for(state, call_deadline, |rt| {
        rt.driver
            .as_ref()
            .map(|d| {
                d.task_id() != id
                    || d.is_terminal()
                    || (d.state() == TaskState::Running && d.pause().is_none())
            })
            .unwrap_or(true)
    })
    .await
    {
        return Ok(notice(
            "paused: waiting for the user in Aperture (resume, confirm, or answer). Call again (no instruction) once they have.",
        ));
    }
    if let Some(reply) = terminal_reply(state, id).await {
        return Ok(reply);
    }

    // B. Claude's instruction for the last screen.
    let now = crate::pipeline::epoch_ms();
    let pending_approved = take_approved_if_current(state, id).await?;
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
                    if let Some(d) = rt.driver.as_mut().filter(|d| d.task_id() == id) {
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
                        image_b64: None,
                        is_error: true,
                    });
                }
            };
            let disposition = {
                let mut rt = state.agent.lock().await;
                let Some(d) = rt.driver.as_mut() else { return Err("task vanished".into()) };
                if d.task_id() != id {
                    return Err(SUPERSEDED.into());
                }
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
                        wait_for(state, call_deadline, |rt| rt.driver.as_ref().map(|d| d.state() != TaskState::Paused).unwrap_or(true)).await;
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
                    let decided = wait_for(state, call_deadline, |rt| {
                        rt.approved_action.is_some()
                            || rt
                                .driver
                                .as_ref()
                                .map(|d| !matches!(d.pause(), Some(PauseReason::Confirm { .. })))
                                .unwrap_or(true)
                    })
                    .await;
                    if !decided {
                        // The chip stays armed in the driver (`pending` is
                        // sticky) — resending the instruction would re-classify
                        // and re-arm a chip the user may meanwhile have skipped
                        // (08-22 review). NO instruction: an approval executes
                        // the pending action on that call, a skip shows up as
                        // last_action "skipped by user".
                        return Ok(notice(format!(
                            "paused: the action {reason} — waiting for the user to approve or skip it. Call again with NO instruction once they have; if approved it runs then, if skipped you'll see that in last_action."
                        )));
                    }
                    if let Some(reply) = terminal_reply(state, id).await {
                        return Ok(reply);
                    }
                    let approved = take_approved_if_current(state, id).await?;
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
            // Identity re-validated after every wait (08-22 review): the
            // driver in the mutex must still be THIS call's task before it is
            // taken out and acted on.
            if !driver_matches(&rt, id) {
                return Err(SUPERSEDED.into());
            }
            let d = rt.driver.take().ok_or_else(|| "task vanished".to_string())?;
            rt.in_flight = true;
            let stopping = rt.stop.load(Ordering::SeqCst) && !d.is_terminal();
            rt.snapshot = Some(render_view(&d, rt.source, true, stopping));
            d
        };
        emit_view(app, state).await;
        let joined = tokio::task::spawn_blocking(move || {
            let mut d = driver;
            // The driver MUST come back whatever happens on this thread: a
            // panic (e.g. a poisoned DB mutex under `expect`) would otherwise
            // be lost with it, leaving in_flight=true forever — the runtime
            // wedged until restart (08-22 review).
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                d.execute(&instr, &action, crate::pipeline::epoch_ms())
            }));
            (d, r)
        })
        .await;
        let (driver, exec_result) = match joined {
            Ok(pair) => pair,
            Err(e) => {
                // The closure itself died without returning the driver (a
                // panic outside catch_unwind, or the task was cancelled): the
                // driver is gone — un-wedge the runtime and fail the DB task
                // rather than leave it "running" forever (08-22 review).
                let now = crate::pipeline::epoch_ms();
                {
                    let mut rt = state.agent.lock().await;
                    rt.in_flight = false;
                    // A pending user stop owns the outcome even here: the task
                    // ends as Cancelled, not misattributed to an internal
                    // failure (08-22 review verify pass).
                    let (terminal, why) = if rt.stop.load(Ordering::SeqCst) {
                        (TaskState::Cancelled, "stopped by user (the executor thread died mid-action)")
                    } else {
                        (TaskState::Failed, "internal error: the executor thread died mid-action")
                    };
                    let _ = rt.tasks().transition(id, terminal, now, Some(why));
                    if let Some(snap) = rt.snapshot.as_mut() {
                        snap.in_flight = false;
                        snap.stopping = false;
                        snap.state = terminal.as_str().to_string();
                        snap.stop_reason = Some(match terminal {
                            TaskState::Cancelled => "stopped by the user".into(),
                            _ => "failed: internal error".into(),
                        });
                        snap.outcome = Some(why.into());
                    }
                }
                notify(state);
                emit_view(app, state).await;
                return Err(format!(
                    "internal error: the executor thread died mid-action ({e}); the task was ended"
                ));
            }
        };
        settle = driver.settle_hint().max(SETTLE_FLOOR);
        let outcome = {
            let mut rt = state.agent.lock().await;
            // Un-wedge FIRST: the driver goes back and in_flight clears
            // before any bookkeeping that can itself panic — hard_stop /
            // fail_internal write rows through the same DB mutex whose
            // poisoning may be the very panic being handled; doing them
            // earlier would drop the driver on the re-panic and wedge the
            // runtime forever (08-22 review verify pass).
            rt.driver = Some(driver);
            rt.in_flight = false;
            let stop_pending = rt.stop.load(Ordering::SeqCst);
            let panicked = exec_result.is_err();
            let outcome = exec_result.ok();
            let book = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let d = rt.driver.as_mut().expect("driver just re-inserted");
                // A hard stop pressed while the action was in flight lands
                // here: apply it BEFORE anything else can happen — the hard
                // stop always wins (locked decision 5; 08-22 review).
                if stop_pending && !d.is_terminal() {
                    let _ = d.hard_stop(crate::pipeline::epoch_ms());
                }
                if panicked && !d.is_terminal() {
                    // The action panicked but the driver survived: a failed
                    // step and a Failed task — never a user hard stop (which
                    // the flag above already applied if pressed).
                    d.fail_internal("the action crashed inside Aperture", crate::pipeline::epoch_ms());
                }
            }));
            if book.is_err() {
                tracing::error!(
                    task = %id,
                    "agent step bookkeeping panicked (poisoned DB?) — runtime un-wedged, task state may lag"
                );
            }
            rt.snapshot = rt.view();
            outcome
        };
        notify(state);
        emit_view(app, state).await;
        let Some(outcome) = outcome else {
            return Ok(StepReply {
                text: "the action crashed inside Aperture — the step failed and the task ended. \
                       No further actions will be taken."
                    .into(),
                image_b64: None,
                is_error: true,
            });
        };
        match outcome {
            Err(e) => return Err(e.to_string()),
            Ok(Err(ActionError::Excluded(_))) | Ok(Err(ActionError::Elevated(_))) => {
                // Decision #49/#50: paused + notified; wait for Resume/Stop.
                if !wait_for_resume(state, id, call_deadline).await {
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

async fn wait_for_resume(
    state: &AppState,
    id: uuid::Uuid,
    call_deadline: tokio::time::Instant,
) -> bool {
    wait_for(state, call_deadline, |rt| {
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
        image_b64: None,
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
    StepReply { text: text.into(), image_b64: None, is_error: false }
}

/// The release gate (08-22 review, findings 1/3): applies a pending hard-stop
/// flag to the driver first (the hard stop always wins), then refuses unless
/// `id` is the current task, Running, and un-paused. `Some` is the reply to
/// send instead of observing/releasing.
async fn refuse_release(
    app: &tauri::AppHandle,
    state: &AppState,
    id: uuid::Uuid,
) -> Option<Result<StepReply, String>> {
    enum Refusal {
        Terminal,
        Paused,
    }
    let refusal = {
        let mut rt = state.agent.lock().await;
        if rt.stop.load(Ordering::SeqCst) {
            if let Some(d) = rt.driver.as_mut() {
                if d.task_id() == id && !d.is_terminal() {
                    let _ = d.hard_stop(crate::pipeline::epoch_ms());
                    rt.snapshot = rt.view();
                }
            }
        }
        let Some(d) = rt.driver.as_ref() else {
            return Some(Err("task vanished".into()));
        };
        if d.task_id() != id {
            return Some(Err(SUPERSEDED.into()));
        }
        if d.is_terminal() {
            Some(Refusal::Terminal)
        } else if d.state() == TaskState::Paused || d.pause().is_some() {
            Some(Refusal::Paused)
        } else {
            None
        }
    };
    match refusal? {
        Refusal::Terminal => {
            finish_side_effects(app, state).await;
            Some(Ok(terminal_reply(state, id)
                .await
                .unwrap_or_else(|| notice("task ended — nothing was observed."))))
        }
        Refusal::Paused => Some(Ok(notice(
            "paused: waiting for the user in Aperture — nothing was observed. Call again (no instruction) after they resume.",
        ))),
    }
}

/// Observe the screen, build + hash the payload, audit, release.
async fn observe_and_release(app: &tauri::AppHandle, state: &AppState, id: uuid::Uuid) -> Result<StepReply, String> {
    // Re-check the driver before any pixel is captured (08-22 review): a hard
    // stop, pause, or superseding task during the settle sleep must win.
    if let Some(reply) = refuse_release(app, state, id).await {
        return reply;
    }
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
    let exclusions = state.exclusions.clone();
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
        // Doc 22 §3.2: open_windows carries APP NAMES ONLY — never window
        // titles (08-22 review: titles shipped unredacted, including excluded
        // windows'). Excluded windows are dropped entirely; a window with no
        // resolvable process name is dropped rather than falling back to its
        // title. Foreground-first order is preserved.
        let mut open: Vec<String> = Vec::new();
        for w in list_open_windows() {
            if matches!(
                exclusions.is_excluded(w.process.as_deref(), None, Some(w.title.as_str()), None),
                aperture_capture::exclusion::ExclusionVerdict::Excluded { .. }
            ) {
                continue;
            }
            let Some(app_name) = w.process else { continue };
            if !open.iter().any(|o| o == &app_name) {
                open.push(app_name);
            }
        }
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
                if let Some(d) = rt.driver.as_mut().filter(|d| d.task_id() == id) {
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
            return Ok(StepReply { text: format!("cannot observe the screen: {e}"), image_b64: None, is_error: true })
        }
    };

    let redactor = Redactor::new(&commands::read_user_redaction_terms(&state.db)).map_err(|e| e.to_string())?;
    let (payload, payload_hash) = build_step_payload(&task_text, step_number, observation, last_action, prior, &redactor);

    // The EXACT wire content, built BEFORE any audit row (08-22 review). What
    // leaves the machine is (a) this final text — the payload JSON with
    // `screenshot_b64` swapped for a marker when the image rides as its own
    // MCP image block, PLUS the trailing call-again instruction — and (b) the
    // screenshot's base64 string, verbatim (`mcp_bridge` forwards both
    // untouched). Two distinct hashes, on purpose:
    // - wire_sha256/byte_count (cloud_send row) = SHA-256 over text bytes ‖
    //   base64-image bytes, and that concatenation's length — the bytes that
    //   actually egress;
    // - `payload_hash` = the structured StepPayload's hash (screenshot_b64
    //   inline), stamped on the next task_steps row as screen_payload_hash
    //   via `d.observed` (locked decision 6).
    let image_b64 = payload.screenshot_b64.clone();
    let mut text_payload = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
    if image_b64.is_some() {
        text_payload["screenshot_b64"] = serde_json::Value::String("<attached as image>".into());
    }
    let text = format!(
        "{}\n\nReply by calling aperture_agent_step again with task_id and your `instruction` for THIS screen.",
        serde_json::to_string(&text_payload).map_err(|e| e.to_string())?
    );
    let (wire_sha256, byte_count) = wire_hash(&text, image_b64.as_deref());

    // Decision #42's hard cap, measured on the true wire size and enforced
    // BEFORE the audit row and BEFORE `observed`: an over-cap screen refuses
    // with no cloud_send row and no step-hash pairing — the audit trail
    // records releases, never refusals (08-22 review: the post-hoc check in
    // mcp_bridge left a phantom "sent" row and under-counted base64).
    if byte_count as usize > MCP_RESULT_MAX_BYTES {
        // The refusal still counts as a step (RK-V2-03): a persistently
        // over-cap screen must not let Claude loop observe→refuse past the
        // step cap forever (08-22 review verify pass).
        {
            let mut rt = state.agent.lock().await;
            if let Some(d) = rt.driver.as_mut() {
                if d.task_id() == id {
                    d.note_noop("screen payload exceeded the MCP result cap — not released", crate::pipeline::epoch_ms());
                }
            }
            rt.snapshot = rt.view();
        }
        notify(state);
        emit_view(app, state).await;
        if let Some(reply) = terminal_reply(state, id).await {
            return Ok(reply);
        }
        return Ok(StepReply {
            text: format!(
                "the screen payload is {byte_count} B — over Claude Desktop (MCP)'s hard cap of {MCP_RESULT_MAX_BYTES} B. Nothing was released (decision #42); this counted as a step."
            ),
            image_b64: None,
            is_error: true,
        });
    }

    // Re-check the driver AGAIN before the audit row and release (08-22
    // review): the observation above took real time — a stop or pause landing
    // in that window discards the observation, releasing nothing.
    if let Some(reply) = refuse_release(app, state, id).await {
        return reply;
    }

    // Audit BEFORE release, fail-closed (doc 13 §3; same as get_context).
    let audit = AuditLog::new(Arc::clone(&state.db));
    if let Err(e) = audit.record_cloud_send(CloudSendRecord {
        payload_id: uuid::Uuid::new_v4(),
        wire_sha256,
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
        if let Some(d) = rt.driver.as_mut().filter(|d| d.task_id() == id) {
            d.observed(payload_hash);
        }
        rt.snapshot = rt.view();
    }
    emit_view(app, state).await;
    tracing::info!(task = %id, step = step_number, bytes = byte_count, "agent step payload released to Claude Desktop (MCP)");
    Ok(StepReply { text, image_b64, is_error: false })
}

/// The agent step's wire identity (08-22 review): SHA-256 over the exact
/// outgoing bytes — the final reply text, then the screenshot's base64 string
/// (when present), concatenated in that order with no separator — plus their
/// combined length. This is what the `cloud_send` row records; the structured
/// StepPayload hash is a separate datum (see `observe_and_release`).
fn wire_hash(text: &str, image_b64: Option<&str>) -> (String, u64) {
    let mut bytes = Vec::with_capacity(text.len() + image_b64.map(str::len).unwrap_or(0));
    bytes.extend_from_slice(text.as_bytes());
    if let Some(b) = image_b64 {
        bytes.extend_from_slice(b.as_bytes());
    }
    (aperture_privacy::audit_log::sha256_hex(&bytes), bytes.len() as u64)
}

/// Build the runtime for `AppState`. The OCR engine is a second
/// `WindowsMediaOcr` instance (cheap; the capture sink owns its own).
pub fn build_runtime(db: Arc<aperture_db::Db>, exclusions: ExclusionList) -> AgentRuntime {
    let ocr: Option<Arc<dyn OcrEngine>> = match aperture_vision_ocr::windows_media_ocr::WindowsMediaOcr::new(
        aperture_vision_ocr::windows_media_ocr::DEFAULT_OCR_LANGUAGE,
    ) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::warn!(error = %e, "agent runtime: Windows OCR unavailable — observations will be text-less");
            None
        }
    };
    let tasks = Arc::new(TaskManager::new(db));
    // Rows a previous process left non-terminal can never resume — the driver
    // died with that process. End them BEFORE the runtime is exposed so the
    // Agent tab never shows a phantom "running" task (08-22 review); step rows
    // are kept (locked decision 6).
    match tasks.reconcile_interrupted(crate::pipeline::epoch_ms()) {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "agent runtime: ended task(s) interrupted by the previous run"),
        Err(e) => tracing::warn!(error = %e, "agent runtime: could not reconcile interrupted tasks"),
    }
    AgentRuntime::new(tasks, ocr, exclusions)
}

/// Hook for the tray's "Stop agent task" item and the command: flag first,
/// then transition.
pub async fn hard_stop_current(app: &tauri::AppHandle, state: &AppState) -> Option<TaskView> {
    let (id, in_flight) = {
        let rt = state.agent.lock().await;
        rt.stop.store(true, Ordering::SeqCst);
        (rt.driver.as_ref().map(|d| d.task_id().to_string()), rt.in_flight)
    };
    match id {
        Some(id) => user_decide(app, state, &id, "stop").await.ok(),
        // The driver is on the blocking thread: the flag is set and is applied
        // the moment the action returns (08-22 review) — acknowledge with the
        // snapshot instead of "no task running", stamped "Stopping…" and
        // broadcast so every overlay's bar says so.
        None if in_flight => {
            let view = {
                let mut rt = state.agent.lock().await;
                if let Some(snap) = rt.snapshot.as_mut() {
                    snap.stopping = true;
                }
                rt.view()
            };
            let _ = events::emit_agent_task(app, view.as_ref());
            view
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_hash_covers_text_then_image_b64_and_counts_both() {
        let (h_both, n_both) = wire_hash("payload text", Some("aGVsbG8="));
        let manual = {
            let mut v = b"payload text".to_vec();
            v.extend_from_slice(b"aGVsbG8=");
            aperture_privacy::audit_log::sha256_hex(&v)
        };
        assert_eq!(h_both, manual, "wire hash = sha256(text bytes ‖ base64 bytes)");
        assert_eq!(n_both, ("payload text".len() + "aGVsbG8=".len()) as u64);
        let (h_text, n_text) = wire_hash("payload text", None);
        assert_ne!(h_both, h_text, "the image is part of the wire identity");
        assert_eq!(n_text, "payload text".len() as u64);
    }

    /// 08-22 review (A1): a Stop carrying a stale task id must not raise the
    /// current task's flag; the in-flight driver is identified by the snapshot.
    #[test]
    fn stop_only_targets_the_current_or_in_flight_task() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let a_s = a.to_string();
        let b_s = b.to_string();
        assert!(stop_targets(false, Some(a), None, a), "the driver in the mutex");
        assert!(!stop_targets(false, Some(b), None, a), "stale id vs the task that replaced it");
        assert!(!stop_targets(false, None, None, a), "no task at all");
        assert!(stop_targets(true, None, Some(&a_s), a), "the driver out on the blocking thread");
        assert!(!stop_targets(true, None, Some(&b_s), a), "stale id vs the in-flight task");
        assert!(!stop_targets(false, None, Some(&a_s), a), "a snapshot alone (dismissed/finished) is no target");
    }

    /// 08-22 review (A3): the executor probe's decision. A rule hit always
    /// refuses; a browser page with no resolvable URL refuses ONLY while
    /// `url_pattern` rules exist (fail closed, decision #49); non-browsers and
    /// resolved-and-allowed pages are untouched.
    #[test]
    fn probe_decision_fails_closed_only_for_unidentified_browser_pages_under_url_rules() {
        let hit = || ExclusionVerdict::Excluded { flags: 0, label: "bank".into() };
        assert_eq!(probe_decision(false, false, None, hit()), Some("bank".into()));
        assert_eq!(probe_decision(true, true, Some("https://bank.example/"), hit()), Some("bank".into()));
        assert_eq!(
            probe_decision(true, true, None, ExclusionVerdict::Allowed),
            Some(BROWSER_PAGE_UNIDENTIFIED.into()),
            "browser + url rules + no URL: pause, never act"
        );
        assert_eq!(probe_decision(true, true, Some("https://docs.rs/"), ExclusionVerdict::Allowed), None);
        assert_eq!(probe_decision(true, false, None, ExclusionVerdict::Allowed), None, "no url rule to protect");
        assert_eq!(probe_decision(false, true, None, ExclusionVerdict::Allowed), None, "non-browser: unchanged");
    }

    /// Decision #42 semantics: exactly at the cap passes, one byte over
    /// refuses — the same boundary `transports::enforce` uses.
    #[test]
    fn wire_cap_boundary_is_measured_on_text_plus_base64() {
        let text = "t".repeat(MCP_RESULT_MAX_BYTES - 8);
        let (_, at_cap) = wire_hash(&text, Some("12345678"));
        assert_eq!(at_cap as usize, MCP_RESULT_MAX_BYTES);
        assert!(at_cap as usize <= MCP_RESULT_MAX_BYTES, "at the cap releases");
        let (_, over) = wire_hash(&text, Some("123456789"));
        assert!(over as usize > MCP_RESULT_MAX_BYTES, "one byte over refuses");
    }
}
