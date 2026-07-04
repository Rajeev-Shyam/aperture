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
    pub suggestions_days: u32,   // 180: suggestions + patterns
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
    pub patterns_deleted: usize,
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

        // 1. ctx_vec rows for expired events (explicit — virtual table, no cascade).
        report.ctx_vec_deleted = conn.execute(
            "DELETE FROM ctx_vec WHERE event_id IN (SELECT id FROM events WHERE ts < ?1)",
            [events_floor],
        ).unwrap_or(0); // tolerate a missing ctx_vec table (vec not loaded)

        // 2. Expired events. screen_context rows cascade (real table, FK ON
        //    DELETE CASCADE). Audit rows (capture_toggle / cloud_send) have their
        //    own TTL and are excluded here; they expire below.
        report.events_deleted = conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND type NOT IN ('capture_toggle','cloud_send')",
            [events_floor],
        )?;

        // 2b. Audit rows expire on their own (longer-lived post-purge window is
        //     handled by Purge-All at M9; day-to-day they follow events_days too,
        //     never shorter than audit_days).
        let audit_floor = now_ms - policy.events_days.max(policy.audit_days) as i64 * DAY_MS;
        conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND type IN ('capture_toggle','cloud_send')",
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

        // 5. Suggestions + patterns (180 d).
        report.suggestions_deleted = conn.execute(
            "DELETE FROM suggestions WHERE COALESCE(resolved_ts, shown_ts, 0) < ?1 \
               AND COALESCE(resolved_ts, shown_ts) IS NOT NULL",
            [sugg_floor],
        )?;
        report.patterns_deleted = conn.execute(
            "DELETE FROM patterns WHERE last_seen IS NOT NULL AND last_seen < ?1",
            [sugg_floor],
        )?;

        // 6. Stale connector state (per-connector TTL, doc 10): anything past its
        //    own stale_after_ts by more than a grace day is dead weight.
        report.connector_state_deleted = conn.execute(
            "DELETE FROM connector_state \
             WHERE stale_after_ts IS NOT NULL AND stale_after_ts < ?1",
            [now_ms - DAY_MS],
        )?;

        conn.execute_batch("COMMIT")?;
        Ok(())
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
}
