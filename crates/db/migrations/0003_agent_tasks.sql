-- v2 skeleton (Doc 22 §3.4): the agent execution layer's task store.
--
-- Additive, forward-only. Nothing in v1 reads these tables; the v2
-- `task-manager` crate owns every access, and v1's proactive pipeline is
-- untouched (Doc 22 locked decision 7 — v1 runs underneath v2 unchanged).
--
-- Auditability is the point (locked decision 6): every step records the
-- SHA-256 of the exact payload sent to Claude, the action taken, and its
-- result — regardless of scoped-allow state. Retention: tasks ride the
-- events window (90 d), task_steps the shorter OCR window (30 d), enforced
-- by the nightly pruner.

CREATE TABLE tasks (
  id              TEXT PRIMARY KEY,            -- uuid
  description     TEXT NOT NULL,               -- the user's stated task
  status          TEXT NOT NULL,               -- idle|running|paused|complete|failed|cancelled
  created_at      INTEGER NOT NULL,            -- epoch ms
  completed_at    INTEGER,                     -- epoch ms, terminal states only
  step_count      INTEGER NOT NULL DEFAULT 0,
  outcome_summary TEXT                         -- Claude's final summary on completion
);

CREATE TABLE task_steps (
  id                  TEXT PRIMARY KEY,        -- uuid
  task_id             TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  step_number         INTEGER NOT NULL,
  screen_payload_hash TEXT,                    -- SHA-256 of what was sent to Claude
  action_type         TEXT,                    -- click|type|key|launch|switch_window|scroll|wait|none
  action_target       TEXT,
  action_value        TEXT,
  result              TEXT,                    -- success|failure|skipped
  claude_reasoning    TEXT,                    -- Claude's stated reason for the action
  timestamp           INTEGER NOT NULL         -- epoch ms
);

CREATE INDEX idx_task_steps_task ON task_steps(task_id, step_number);
CREATE INDEX idx_tasks_status ON tasks(status, created_at);
