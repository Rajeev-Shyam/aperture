//! Retention & lifecycle (doc 03 §6, doc 13 §7). A nightly job enforces TTLs.
//! Defaults are user-configurable in `settings`.
//!
//! Runs on startup and on a daily timer (doc 16 M2 / build prompt). Purge-All
//! (the one-click nuke with audit survival) is separate and lands at M9.

use crate::{Db, DbError};

/// Default TTLs in days (doc 03 §6, Q73: unchanged in R2). All [ASSUMPTION] in
/// the spec; user-adjustable.
pub struct RetentionPolicy {
    pub events_days: u32,        // 90: events + ctx_vec (vec rows cascade)
    pub ocr_text_days: u32,      // 30: nullify ocr_text, keep event skeleton
    pub voice_days: u32,         // 30: voice_utterance transcript scrub
    pub suggestions_days: u32,   // 180: suggestions (patterns have their own owner — see below)
    pub audit_days: u32,         // 30: capture_toggle + cloud_send survive purge this long
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            events_days: 90,
            ocr_text_days: 30,
            voice_days: 30,
            suggestions_days: 180,
            audit_days: 30,
        }
    }
}

/// What one prune pass removed (logged + fed to gate telemetry).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub events_deleted: usize,
    pub ctx_vec_deleted: usize,
    pub ocr_text_nullified: usize,
    pub voice_scrubbed: usize,
    pub suggestions_deleted: usize,
    pub connector_state_deleted: usize,
}

const DAY_MS: i64 = 86_400_000;

/// Run the nightly pruner (doc 03 §6). Raw frames are never persisted, so there
/// is nothing to prune there (doc 13 §4).
///
/// Order matters: `ctx_vec` rows are deleted alongside their events explicitly —
/// `ctx_vec` is a `vec0` virtual table, so the `events` FK cascade does **not**
/// reach it ([VERIFY resolved — Step 0]: sqlite-vec virtual tables don't
/// participate in FK cascades; the doc 03 "vec rows cascade" is implemented
/// here, in the same transaction, instead).
pub fn run_nightly_prune(db: &Db, now_ms: i64, policy: &RetentionPolicy) -> Result<PruneReport, DbError> {
    let events_floor = now_ms - policy.events_days as i64 * DAY_MS;
    let ocr_floor = now_ms - policy.ocr_text_days as i64 * DAY_MS;
    let voice_floor = now_ms - policy.voice_days as i64 * DAY_MS;
    let sugg_floor = now_ms - policy.suggestions_days as i64 * DAY_MS;

    let mut report = PruneReport::default();

    db.with_conn(|conn| {
        conn.execute_batch("BEGIN")?;
        // The connection is shared (single Mutex<Connection>): an error that
        // escapes with BEGIN still open would silently swallow every later
        // write into the dead transaction — ROLLBACK before propagating, same
        // as migrations::run.
        let pruned = (|| -> Result<(), rusqlite::Error> {
        // 1. ctx_vec rows for expired events (explicit — virtual table, no cascade).
        report.ctx_vec_deleted = conn.execute(
            "DELETE FROM ctx_vec WHERE event_id IN (SELECT id FROM events WHERE ts < ?1)",
            [events_floor],
        ).unwrap_or(0); // tolerate a missing ctx_vec table (vec not loaded)

        // 2. Expired events. screen_context rows cascade (real table, FK ON
        //    DELETE CASCADE). Audit rows (capture_toggle / cloud_send) have their
        //    own TTL and are excluded here; they expire below.
        report.events_deleted = conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND type NOT IN ('capture_toggle','cloud_send','mcp_search')",
            [events_floor],
        )?;

        // 2b. Audit rows expire on their own (longer-lived post-purge window is
        //     handled by Purge-All at M9; day-to-day they follow events_days too,
        //     never shorter than audit_days).
        let audit_floor = now_ms - policy.events_days.max(policy.audit_days) as i64 * DAY_MS;
        conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND type IN ('capture_toggle','cloud_send','mcp_search')",
            [audit_floor],
        )?;

        // 3. OCR text: nullify text, keep the event skeleton (doc 03 §6).
        report.ocr_text_nullified = conn.execute(
            "UPDATE screen_context SET ocr_text = NULL \
             WHERE ocr_text IS NOT NULL \
               AND event_id IN (SELECT id FROM events WHERE ts < ?1)",
            [ocr_floor],
        )?;

        // 4. Voice transcripts: scrub the payload, keep the event (doc 03 §6).
        report.voice_scrubbed = conn.execute(
            "UPDATE events SET payload = json_object('scrubbed', 1) \
             WHERE type = 'voice_utterance' AND ts < ?1 \
               AND json_extract(payload, '$.scrubbed') IS NULL",
            [voice_floor],
        )?;

        // 5. Suggestions (180 d). `patterns` is deliberately NOT touched here:
        //    the pattern engine's decay prune is the SOLE owner of pattern-row
        //    deletion (owner decision #18, 2026-08-16) — the pattern task runs
        //    it daily and mirrors the result to the DB (src-tauri pipeline.rs).
        //    The age rule this job used to apply was strictly dominated by the
        //    decay rule, and two uncoordinated deleters on two timers could
        //    disagree. Purge-All still nukes patterns directly — that is the
        //    user's explicit action, not a retention policy.
        report.suggestions_deleted = conn.execute(
            "DELETE FROM suggestions WHERE COALESCE(resolved_ts, shown_ts, 0) < ?1 \
               AND COALESCE(resolved_ts, shown_ts) IS NOT NULL",
            [sugg_floor],
        )?;

        // 6. Stale connector state (per-connector TTL, doc 10): anything past its
        //    own stale_after_ts by more than a grace day is dead weight. Detach
        //    the longer-lived rows that reference it FIRST (events 90 d,
        //    suggestions 180 d — and every decision-#15 `app_focus` row is born
        //    referenced by its suggestion): with `foreign_keys=ON` a referenced
        //    parent delete trips the FK and rolls back the whole pass.
        let stale_floor = now_ms - DAY_MS;
        conn.execute(
            "UPDATE events SET connector_id = NULL WHERE connector_id IN \
             (SELECT id FROM connector_state \
              WHERE stale_after_ts IS NOT NULL AND stale_after_ts < ?1)",
            [stale_floor],
        )?;
        conn.execute(
            "UPDATE suggestions SET connector_id = NULL WHERE connector_id IN \
             (SELECT id FROM connector_state \
              WHERE stale_after_ts IS NOT NULL AND stale_after_ts < ?1)",
            [stale_floor],
        )?;
        report.connector_state_deleted = conn.execute(
            "DELETE FROM connector_state \
             WHERE stale_after_ts IS NOT NULL AND stale_after_ts < ?1",
            [stale_floor],
        )?;

        // 7. v2 agent tables (Doc 22 §3.4): steps expire on the shorter OCR
        //    window (they carry action text), tasks on the events window;
        //    a task's remaining steps cascade with it.
        conn.execute("DELETE FROM task_steps WHERE timestamp < ?1", [ocr_floor])?;
        conn.execute("DELETE FROM tasks WHERE created_at < ?1", [events_floor])?;
        Ok(())
        })();

        match pruned {
            Ok(()) => conn.execute_batch("COMMIT"),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    })?;

    tracing::info!(?report, "retention prune complete");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::{Event, EventType};

    fn ev_at(ts: i64, ty: EventType) -> Event {
        Event {
            id: 0,
            ts,
            r#type: ty,
            app: None,
            process: None,
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn prune_deletes_expired_events_but_keeps_recent_and_audit() {
        let db = Db::open_in_memory().expect("open");
        let now = 100 * DAY_MS + 1_700_000_000_000;
        let old = now - 95 * DAY_MS; // past the 90 d TTL
        let fresh = now - DAY_MS;

        let old_id = db.insert_event(&ev_at(old, EventType::WindowFocus)).unwrap();
        let fresh_id = db.insert_event(&ev_at(fresh, EventType::WindowFocus)).unwrap();
        // An old audit row inside the audit window logic (events_days applies).
        let audit_recent_id = db.insert_event(&ev_at(fresh, EventType::CaptureToggle)).unwrap();

        let report = run_nightly_prune(&db, now, &RetentionPolicy::default()).unwrap();
        assert_eq!(report.events_deleted, 1);
        assert!(db.read_event(old_id).is_err(), "expired event deleted");
        assert!(db.read_event(fresh_id).is_ok(), "fresh event kept");
        assert!(db.read_event(audit_recent_id).is_ok(), "audit row kept");
    }

    #[test]
    fn prune_nullifies_old_ocr_text_but_keeps_event_skeleton() {
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000 + 100 * DAY_MS;
        let old = now - 40 * DAY_MS; // past 30 d OCR TTL, inside 90 d events TTL

        let ctx = crate::ScreenContextInsert {
            ocr_text: Some("sensitive text".into()),
            ocr_confidence: Some(0.8),
            ..Default::default()
        };
        let id = db
            .insert_event_with_context(&ev_at(old, EventType::WindowFocus), Some(&ctx), None)
            .unwrap();

        let report = run_nightly_prune(&db, now, &RetentionPolicy::default()).unwrap();
        assert_eq!(report.ocr_text_nullified, 1);
        assert!(db.read_event(id).is_ok(), "event skeleton survives");
        let text: Option<String> = db
            .with_conn(|c| {
                c.query_row(
                    "SELECT ocr_text FROM screen_context WHERE event_id = ?1",
                    [id],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(text, None, "ocr_text nullified");
    }

    #[test]
    fn retention_never_touches_patterns_the_engine_prune_owns_them() {
        // Owner decision #18 (2026-08-16): ONE pattern-prune owner — the
        // engine's decay prune (mirrored by the pattern task). Even an
        // ancient pattern row must survive the retention job.
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000 + 400 * DAY_MS;
        let ancient = now - 365 * DAY_MS; // far past the old 180 d age rule
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO patterns (signature, n, support, confidence, last_seen) \
                 VALUES ('a:focus:x ⇒ b:focus:y', 2, 3, 0.9, ?1)",
                [ancient],
            )
            .map(|_| ())
        })
        .unwrap();

        run_nightly_prune(&db, now, &RetentionPolicy::default()).unwrap();

        let survivors: i64 = db
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM patterns", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(survivors, 1, "retention must leave patterns to the engine prune (#18)");
    }

    #[test]
    fn stale_connector_state_prunes_even_when_referenced() {
        // A stale connector row referenced by a younger suggestion row and
        // event (the normal case: suggestions live 180 d, connector rows days —
        // and every `app_focus` row from decision #15 is born referenced).
        // The prune must detach the references and delete the row, not trip
        // the FK and roll back the whole pass.
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000 + 100 * DAY_MS;
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO connector_state (id, connector_type, reconstruct_payload, captured_ts, stale_after_ts) \
                 VALUES ('cs-stale', 'app_focus', '{}', ?1, ?2)",
                [now - 10 * DAY_MS, now - 5 * DAY_MS],
            )?;
            c.execute(
                "INSERT INTO suggestions (connector_id, source, title, state, shown_ts) \
                 VALUES ('cs-stale', 'local', 'Switch to X', 'shown', ?1)",
                [now - 5 * DAY_MS],
            )?;
            c.execute(
                "INSERT INTO events (ts, type, payload, connector_id) \
                 VALUES (?1, 'suggestion_clicked', '{}', 'cs-stale')",
                [now - 5 * DAY_MS],
            )
            .map(|_| ())
        })
        .unwrap();

        let report = run_nightly_prune(&db, now, &RetentionPolicy::default())
            .expect("prune must survive referenced stale connector rows");
        assert_eq!(report.connector_state_deleted, 1);
        let (sugg_ref, ev_ref): (Option<String>, Option<String>) = db
            .with_conn(|c| {
                let s = c.query_row("SELECT connector_id FROM suggestions", [], |r| r.get(0))?;
                let e = c.query_row("SELECT connector_id FROM events", [], |r| r.get(0))?;
                Ok((s, e))
            })
            .unwrap();
        assert_eq!(sugg_ref, None, "suggestion detached, row kept");
        assert_eq!(ev_ref, None, "event detached, row kept");
    }

    #[test]
    fn prune_error_rolls_back_and_frees_the_shared_connection() {
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000 + 100 * DAY_MS;
        // Force a mid-prune statement failure after BEGIN.
        db.with_conn(|c| c.execute_batch("DROP TABLE suggestions").map(|_| ()))
            .unwrap();

        assert!(run_nightly_prune(&db, now, &RetentionPolicy::default()).is_err());

        // The failed prune must not leave BEGIN open: a write that opens its
        // own transaction (insert_event_with_context) still succeeds, and its
        // row is durable (not silently joined to a dead transaction).
        let id = db
            .insert_event_with_context(&ev_at(now, EventType::WindowFocus), None, None)
            .expect("connection free after failed prune");
        assert!(db.read_event(id).is_ok());
    }
}
