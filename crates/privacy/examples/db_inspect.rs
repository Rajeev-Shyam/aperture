//! Read-only inspector for an Aperture history DB (owner diagnostics).
//!
//! Opens the DB with the install's own DPAPI-wrapped key (doc 13 §6) and prints
//! what the proactive path has actually accumulated: events by type and day,
//! learned patterns, connector states, suggestions, exclusions, settings and
//! the capture-toggle timeline. Nothing is written except SQLCipher's own
//! WAL bookkeeping, so run it on a COPY of the DB when the app is running.
//!
//! ```text
//! cargo run -p aperture-privacy --features aperture-db/sqlcipher --example db_inspect -- <path\to\history.db>
//! ```
//! With no path it opens `aperture_db::default_db_path()` (the live DB).

use std::path::PathBuf;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(aperture_db::default_db_path);
    let key = aperture_privacy::key_manager::get_or_create_key().expect("DB key (Credential Manager + DPAPI)");
    let db = aperture_db::Db::open_encrypted(path.clone(), key.as_bytes()).expect("open history DB");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    println!("== {} (encrypted: {}) ==", path.display(), db.is_encrypted());
    // Ad-hoc mode: `db_inspect <path> "<sql>" <ncols>` runs one query and exits.
    if let (Some(sql), Some(n)) = (std::env::args().nth(2), std::env::args().nth(3)) {
        section(&db, "ad-hoc", &sql, n.parse().unwrap_or(1));
        return;
    }

    section(&db, "events: count / first / last", "SELECT CAST(COUNT(*) AS TEXT), datetime(MIN(ts)/1000,'unixepoch','localtime'), datetime(MAX(ts)/1000,'unixepoch','localtime') FROM events", 3);
    section(&db, "events by type", "SELECT type, CAST(COUNT(*) AS TEXT) FROM events GROUP BY type ORDER BY COUNT(*) DESC", 2);
    section(&db, "behavioural events per day (last 21 days)", "SELECT date(ts/1000,'unixepoch','localtime') AS d, CAST(COUNT(*) AS TEXT), CAST(COUNT(DISTINCT session_id) AS TEXT) FROM events WHERE type IN ('window_focus','window_open','navigation','media_state','document_state','ide_state') GROUP BY d ORDER BY d DESC LIMIT 21", 3);
    section(&db, "top processes (focus events)", "SELECT COALESCE(process,'<null>'), CAST(COUNT(*) AS TEXT), CAST(SUM(redaction_flags) AS TEXT) FROM events WHERE type='window_focus' GROUP BY process ORDER BY COUNT(*) DESC LIMIT 20", 3);
    section(&db, "session stamping", "SELECT CAST(SUM(session_id IS NULL) AS TEXT), CAST(COUNT(DISTINCT session_id) AS TEXT), CAST(MAX(session_id) AS TEXT) FROM events", 3);
    section(&db, "screen_context: rows / mean OCR conf / with VLM summary", "SELECT CAST(COUNT(*) AS TEXT), printf('%.3f', AVG(ocr_confidence)), CAST(SUM(vlm_summary IS NOT NULL) AS TEXT) FROM screen_context", 3);
    section(&db, "patterns: count", "SELECT CAST(COUNT(*) AS TEXT), CAST(SUM(support >= 3) AS TEXT) FROM patterns", 2);
    section(&db, "patterns: top 30 by support", "SELECT CAST(id AS TEXT), signature, CAST(support AS TEXT), printf('%.2f', confidence), datetime(last_seen/1000,'unixepoch','localtime'), printf('%.2f', dismiss_decay), COALESCE(CAST(muted_until AS TEXT),'') FROM patterns ORDER BY support DESC, confidence DESC LIMIT 30", 7);
    section(&db, "connector_state by type: total / fresh now", &format!("SELECT connector_type, CAST(COUNT(*) AS TEXT), CAST(SUM(stale_after_ts IS NULL OR stale_after_ts > {now_ms}) AS TEXT), datetime(MAX(captured_ts)/1000,'unixepoch','localtime') FROM connector_state GROUP BY connector_type"), 4);
    section(&db, "suggestions by source/state", "SELECT source, COALESCE(state,'<null>'), CAST(COUNT(*) AS TEXT) FROM suggestions GROUP BY source, state", 3);
    section(&db, "suggestions: last 15", "SELECT CAST(id AS TEXT), COALESCE(title,''), printf('%.2f', confidence), COALESCE(state,''), COALESCE(outcome,''), datetime(COALESCE(created_ts, shown_ts)/1000,'unixepoch','localtime') FROM suggestions ORDER BY id DESC LIMIT 15", 6);
    section(&db, "exclusion_list", "SELECT match_kind, pattern, CAST(enabled AS TEXT) FROM exclusion_list", 3);
    section(&db, "settings", "SELECT key, substr(value,1,240) FROM settings ORDER BY key", 2);
    section(&db, "capture_toggle timeline (last 20)", "SELECT datetime(ts/1000,'unixepoch','localtime'), COALESCE(payload,'') FROM events WHERE type='capture_toggle' ORDER BY ts DESC LIMIT 20", 2);
    section(&db, "last 25 behavioural events", "SELECT datetime(ts/1000,'unixepoch','localtime'), type, COALESCE(process,''), substr(COALESCE(window_title,''),1,60), COALESCE(CAST(session_id AS TEXT),'-'), substr(COALESCE(payload,''),1,80) FROM events WHERE type IN ('window_focus','window_open','navigation','media_state','document_state','ide_state') ORDER BY ts DESC LIMIT 25", 6);
}

fn section(db: &aperture_db::Db, title: &str, sql: &str, ncols: usize) {
    println!("\n-- {title}");
    let rows = db.with_conn(|c| {
        let mut stmt = c.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| {
                let mut cols = Vec::with_capacity(ncols);
                for i in 0..ncols {
                    cols.push(r.get::<_, Option<String>>(i)?.unwrap_or_else(|| "NULL".into()));
                }
                Ok(cols)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    });
    match rows {
        Ok(rows) if rows.is_empty() => println!("(no rows)"),
        Ok(rows) => {
            for row in rows {
                println!("{}", row.join(" | "));
            }
        }
        Err(e) => println!("query failed: {e}"),
    }
}
