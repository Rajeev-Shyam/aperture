//! Task store (Doc 22 §3.4) — **v2 SKELETON**, DB-backed and fully tested.
//!
//! Owns every access to the `tasks` / `task_steps` tables (migration 0003).
//! v1 code never touches them (locked decision 7). The state-transition rules
//! live in `agent-loop`'s state machine; this crate enforces the same rules at
//! the persistence boundary so a buggy caller cannot write an illegal
//! transition into the durable record.
//!
//! Audit posture (locked decision 6): a step row is written for EVERY step —
//! payload hash + action + result — regardless of scoped-allow state, and
//! steps are append-only (no update/delete API; retention prunes by age).

use aperture_contracts::agent::{StepResult, TaskState};
use aperture_db::Db;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("db: {0}")]
    Db(String),
    #[error("unknown task {0}")]
    UnknownTask(uuid::Uuid),
    #[error("illegal transition {from:?} -> {to:?} for task {task}")]
    IllegalTransition {
        task: uuid::Uuid,
        from: TaskState,
        to: TaskState,
    },
}

/// One `tasks` row (Doc 22 §3.4).
#[derive(Debug, Clone)]
pub struct Task {
    pub id: uuid::Uuid,
    pub description: String,
    pub status: TaskState,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub step_count: i64,
    pub outcome_summary: Option<String>,
}

/// One append-only `task_steps` row (Doc 22 §3.4).
#[derive(Debug, Clone)]
pub struct StepRecord {
    pub task_id: uuid::Uuid,
    pub step_number: u32,
    /// SHA-256 of the exact payload staged for Claude (screen-serializer).
    pub screen_payload_hash: Option<String>,
    pub action_type: Option<String>,
    pub action_target: Option<String>,
    pub action_value: Option<String>,
    pub result: Option<StepResult>,
    pub claude_reasoning: Option<String>,
    pub timestamp: i64,
}

/// The outcome stamped on rows [`TaskManager::reconcile_interrupted`] ends.
pub const INTERRUPTED_SUMMARY: &str = "interrupted: Aperture restarted";

/// Is `from -> to` a legal Doc 22 §3.3 transition?
/// `IDLE → RUNNING → (PAUSED | COMPLETE | FAILED | CANCELLED)`, `PAUSED`
/// resumes to `RUNNING` or terminates; terminal states never move again.
pub fn transition_is_legal(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;
    match (from, to) {
        (Idle, Running) => true,
        (Running, Paused | Complete | Failed | Cancelled) => true,
        (Paused, Running | Complete | Failed | Cancelled) => true,
        // A cancel must always be honorable, even pre-start (hard stop,
        // locked decision 5).
        (Idle, Cancelled) => true,
        _ => false,
    }
}

/// The store facade. Cheap to clone-by-reference (`&TaskManager`); holds the
/// shared encrypted-DB handle.
pub struct TaskManager {
    db: std::sync::Arc<Db>,
}

impl TaskManager {
    pub fn new(db: std::sync::Arc<Db>) -> Self {
        Self { db }
    }

    /// Create a task in `Idle` (Doc 22 §9.1: stored immediately on submission).
    pub fn create_task(&self, description: &str, now_ms: i64) -> Result<Task, TaskError> {
        let task = Task {
            id: uuid::Uuid::new_v4(),
            description: description.to_string(),
            status: TaskState::Idle,
            created_at: now_ms,
            completed_at: None,
            step_count: 0,
            outcome_summary: None,
        };
        self.db
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO tasks (id, description, status, created_at, step_count) \
                     VALUES (?1, ?2, ?3, ?4, 0)",
                    rusqlite::params![
                        task.id.to_string(),
                        task.description,
                        task.status.as_str(),
                        task.created_at,
                    ],
                )
                .map(|_| ())
            })
            .map_err(|e| TaskError::Db(e.to_string()))?;
        Ok(task)
    }

    /// Read one task.
    pub fn get_task(&self, id: uuid::Uuid) -> Result<Task, TaskError> {
        self.db
            .with_conn(|c| {
                c.query_row(
                    "SELECT id, description, status, created_at, completed_at, step_count, \
                            outcome_summary FROM tasks WHERE id = ?1",
                    [id.to_string()],
                    row_to_task,
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            })
            .map_err(|e| TaskError::Db(e.to_string()))?
            .ok_or(TaskError::UnknownTask(id))
    }

    /// Apply a state transition, enforcing Doc 22 §3.3 legality at the
    /// persistence boundary. Terminal transitions stamp `completed_at` and,
    /// for `Complete`, the outcome summary.
    pub fn transition(
        &self,
        id: uuid::Uuid,
        to: TaskState,
        now_ms: i64,
        outcome_summary: Option<&str>,
    ) -> Result<Task, TaskError> {
        let current = self.get_task(id)?;
        if !transition_is_legal(current.status, to) {
            return Err(TaskError::IllegalTransition { task: id, from: current.status, to });
        }
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE tasks SET status = ?2, \
                     completed_at = CASE WHEN ?3 THEN ?4 ELSE completed_at END, \
                     outcome_summary = COALESCE(?5, outcome_summary) \
                     WHERE id = ?1",
                    rusqlite::params![
                        id.to_string(),
                        to.as_str(),
                        to.is_terminal(),
                        now_ms,
                        outcome_summary,
                    ],
                )
                .map(|_| ())
            })
            .map_err(|e| TaskError::Db(e.to_string()))?;
        self.get_task(id)
    }

    /// Startup reconciliation (08-22 review): rows a previous process left
    /// non-terminal (a crash or quit mid-task) can never resume — the driver
    /// died with that process — so they are ended here, through the same
    /// legality gate as every other transition: `Idle` (never started) →
    /// `Cancelled`, `Running` / `Paused` → `Failed`, each stamped
    /// [`INTERRUPTED_SUMMARY`]. Step rows are never touched (locked decision
    /// 6: the audit trail is what the row preserves). Returns how many rows
    /// were reconciled; a second pass finds nothing.
    pub fn reconcile_interrupted(&self, now_ms: i64) -> Result<usize, TaskError> {
        let live: Vec<(uuid::Uuid, TaskState)> = self
            .db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, status FROM tasks WHERE status IN ('idle', 'running', 'paused')",
                )?;
                let rows = stmt.query_map([], |r| {
                    let id: String = r.get(0)?;
                    let status: String = r.get(1)?;
                    Ok((uuid::Uuid::parse_str(&id).unwrap_or_default(), parse_state(&status)))
                })?;
                rows.collect()
            })
            .map_err(|e| TaskError::Db(e.to_string()))?;
        let mut reconciled = 0;
        for (id, from) in live {
            let to = match from {
                TaskState::Idle => TaskState::Cancelled,
                _ => TaskState::Failed,
            };
            self.transition(id, to, now_ms, Some(INTERRUPTED_SUMMARY))?;
            reconciled += 1;
        }
        Ok(reconciled)
    }

    /// Append one step's audit row and bump the task's `step_count` — one
    /// transaction, append-only (locked decision 6).
    pub fn record_step(&self, step: &StepRecord) -> Result<(), TaskError> {
        let step_id = uuid::Uuid::new_v4();
        self.db
            .with_conn(|c| {
                let tx = c.unchecked_transaction()?;
                tx.execute(
                    "INSERT INTO task_steps (id, task_id, step_number, screen_payload_hash, \
                     action_type, action_target, action_value, result, claude_reasoning, timestamp) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        step_id.to_string(),
                        step.task_id.to_string(),
                        step.step_number,
                        step.screen_payload_hash,
                        step.action_type,
                        step.action_target,
                        step.action_value,
                        step.result.map(StepResult::as_str),
                        step.claude_reasoning,
                        step.timestamp,
                    ],
                )?;
                tx.execute(
                    "UPDATE tasks SET step_count = step_count + 1 WHERE id = ?1",
                    [step.task_id.to_string()],
                )?;
                tx.commit()?;
                Ok(())
            })
            .map_err(|e| TaskError::Db(e.to_string()))
    }

    /// Recent tasks, newest first (the V2-M6 history view, Dashboard Agent tab).
    pub fn list_tasks(&self, limit: u32) -> Result<Vec<Task>, TaskError> {
        self.db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, description, status, created_at, completed_at, step_count, \
                            outcome_summary FROM tasks ORDER BY created_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit.max(1)], row_to_task)?;
                rows.collect()
            })
            .map_err(|e| TaskError::Db(e.to_string()))
    }

    /// A task's steps, oldest first (the V2-M6 history/audit view reads this).
    pub fn steps(&self, task_id: uuid::Uuid) -> Result<Vec<StepRecord>, TaskError> {
        self.db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT task_id, step_number, screen_payload_hash, action_type, \
                            action_target, action_value, result, claude_reasoning, timestamp \
                     FROM task_steps WHERE task_id = ?1 ORDER BY step_number ASC",
                )?;
                let rows = stmt.query_map([task_id.to_string()], row_to_step)?;
                rows.collect()
            })
            .map_err(|e| TaskError::Db(e.to_string()))
    }

    /// Purge one task and its steps — the "Purge task history" mitigation
    /// (Doc 22 §8); steps cascade via the FK.
    pub fn purge_task(&self, task_id: uuid::Uuid) -> Result<(), TaskError> {
        self.db
            .with_conn(|c| {
                c.execute("DELETE FROM tasks WHERE id = ?1", [task_id.to_string()])
                    .map(|_| ())
            })
            .map_err(|e| TaskError::Db(e.to_string()))
    }
}

fn row_to_task(r: &rusqlite::Row<'_>) -> Result<Task, rusqlite::Error> {
    let id: String = r.get(0)?;
    let status: String = r.get(2)?;
    Ok(Task {
        id: uuid::Uuid::parse_str(&id).unwrap_or_default(),
        description: r.get(1)?,
        status: parse_state(&status),
        created_at: r.get(3)?,
        completed_at: r.get(4)?,
        step_count: r.get(5)?,
        outcome_summary: r.get(6)?,
    })
}

fn row_to_step(r: &rusqlite::Row<'_>) -> Result<StepRecord, rusqlite::Error> {
    let task_id: String = r.get(0)?;
    let result: Option<String> = r.get(6)?;
    Ok(StepRecord {
        task_id: uuid::Uuid::parse_str(&task_id).unwrap_or_default(),
        step_number: r.get(1)?,
        screen_payload_hash: r.get(2)?,
        action_type: r.get(3)?,
        action_target: r.get(4)?,
        action_value: r.get(5)?,
        result: result.as_deref().and_then(|s| match s {
            "success" => Some(StepResult::Success),
            "failure" => Some(StepResult::Failure),
            "skipped" => Some(StepResult::Skipped),
            _ => None,
        }),
        claude_reasoning: r.get(7)?,
        timestamp: r.get(8)?,
    })
}

fn parse_state(s: &str) -> TaskState {
    match s {
        "running" => TaskState::Running,
        "paused" => TaskState::Paused,
        "complete" => TaskState::Complete,
        "failed" => TaskState::Failed,
        "cancelled" => TaskState::Cancelled,
        _ => TaskState::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mgr() -> TaskManager {
        TaskManager::new(Arc::new(Db::open_in_memory().expect("db")))
    }

    #[test]
    fn create_transition_and_read_back() {
        let m = mgr();
        let t = m.create_task("file my expenses", 1_000).expect("create");
        assert_eq!(t.status, TaskState::Idle);

        let t = m.transition(t.id, TaskState::Running, 2_000, None).expect("start");
        assert_eq!(t.status, TaskState::Running);
        assert_eq!(t.completed_at, None);

        let t = m
            .transition(t.id, TaskState::Complete, 3_000, Some("expenses filed"))
            .expect("complete");
        assert_eq!(t.status, TaskState::Complete);
        assert_eq!(t.completed_at, Some(3_000));
        assert_eq!(t.outcome_summary.as_deref(), Some("expenses filed"));
    }

    #[test]
    fn illegal_transitions_are_refused_at_the_persistence_boundary() {
        let m = mgr();
        let t = m.create_task("t", 0).unwrap();
        // Idle -> Complete skips Running: illegal.
        assert!(matches!(
            m.transition(t.id, TaskState::Complete, 1, None),
            Err(TaskError::IllegalTransition { .. })
        ));
        // Terminal states never move again (hard stop writes Cancelled once).
        m.transition(t.id, TaskState::Cancelled, 1, None).unwrap();
        assert!(matches!(
            m.transition(t.id, TaskState::Running, 2, None),
            Err(TaskError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn steps_append_and_bump_the_counter() {
        let m = mgr();
        let t = m.create_task("t", 0).unwrap();
        for n in 1..=3u32 {
            m.record_step(&StepRecord {
                task_id: t.id,
                step_number: n,
                screen_payload_hash: Some(format!("hash{n}")),
                action_type: Some("click".into()),
                action_target: Some("Submit".into()),
                action_value: None,
                result: Some(StepResult::Success),
                claude_reasoning: Some("because".into()),
                timestamp: n as i64 * 100,
            })
            .expect("step");
        }
        assert_eq!(m.get_task(t.id).unwrap().step_count, 3);
        let steps = m.steps(t.id).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].screen_payload_hash.as_deref(), Some("hash1"));
        assert_eq!(steps[2].step_number, 3);
    }

    #[test]
    fn purge_task_removes_steps_via_cascade() {
        let m = mgr();
        let t = m.create_task("t", 0).unwrap();
        m.record_step(&StepRecord {
            task_id: t.id,
            step_number: 1,
            screen_payload_hash: None,
            action_type: None,
            action_target: None,
            action_value: None,
            result: None,
            claude_reasoning: None,
            timestamp: 1,
        })
        .unwrap();
        m.purge_task(t.id).expect("purge");
        assert!(matches!(m.get_task(t.id), Err(TaskError::UnknownTask(_))));
        assert!(m.steps(t.id).unwrap().is_empty(), "steps cascade with the task");
    }

    #[test]
    fn list_tasks_is_newest_first_and_bounded() {
        let m = mgr();
        for (i, d) in ["a", "b", "c"].iter().enumerate() {
            m.create_task(d, i as i64 * 10).unwrap();
        }
        let all = m.list_tasks(10).unwrap();
        assert_eq!(all.iter().map(|t| t.description.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(m.list_tasks(2).unwrap().len(), 2);
    }

    /// 08-22 review: a crash/quit leaves idle/running/paused rows behind; on
    /// the next launch every one of them is ended through the legal path,
    /// terminal rows are untouched, and step rows survive (locked decision 6).
    #[test]
    fn reconcile_interrupted_ends_non_terminal_rows_and_keeps_steps() {
        let m = mgr();
        let idle = m.create_task("idle", 0).unwrap();
        let running = m.create_task("running", 0).unwrap();
        m.transition(running.id, TaskState::Running, 1, None).unwrap();
        let paused = m.create_task("paused", 0).unwrap();
        m.transition(paused.id, TaskState::Running, 1, None).unwrap();
        m.transition(paused.id, TaskState::Paused, 2, None).unwrap();
        let done = m.create_task("done", 0).unwrap();
        m.transition(done.id, TaskState::Running, 1, None).unwrap();
        m.transition(done.id, TaskState::Complete, 2, Some("finished")).unwrap();
        m.record_step(&StepRecord {
            task_id: running.id,
            step_number: 1,
            screen_payload_hash: Some("h1".into()),
            action_type: Some("click".into()),
            action_target: None,
            action_value: None,
            result: Some(StepResult::Success),
            claude_reasoning: None,
            timestamp: 3,
        })
        .unwrap();

        assert_eq!(m.reconcile_interrupted(9_000).unwrap(), 3);

        let idle = m.get_task(idle.id).unwrap();
        assert_eq!(idle.status, TaskState::Cancelled, "never started → cancelled (Idle→Failed is illegal)");
        assert_eq!(idle.outcome_summary.as_deref(), Some(INTERRUPTED_SUMMARY));
        assert_eq!(idle.completed_at, Some(9_000));
        let running = m.get_task(running.id).unwrap();
        assert_eq!(running.status, TaskState::Failed);
        assert_eq!(running.outcome_summary.as_deref(), Some(INTERRUPTED_SUMMARY));
        assert_eq!(m.get_task(paused.id).unwrap().status, TaskState::Failed);
        let done = m.get_task(done.id).unwrap();
        assert_eq!(done.status, TaskState::Complete, "terminal rows never move");
        assert_eq!(done.outcome_summary.as_deref(), Some("finished"));
        assert_eq!(m.steps(running.id).unwrap().len(), 1, "steps are never deleted");
        assert_eq!(running.step_count, 1);
        assert_eq!(m.reconcile_interrupted(9_001).unwrap(), 0, "idempotent");
    }

    #[test]
    fn unknown_task_is_a_typed_error() {
        assert!(matches!(
            mgr().get_task(uuid::Uuid::new_v4()),
            Err(TaskError::UnknownTask(_))
        ));
    }
}
