//! Storage layer (doc 03): SQLite + sqlite-vec, the single durable backbone.
//!
//! The bus (doc 15 §1) is at-most-once; **this is the truth**. Only the Tier-0
//! pipeline writes; the pattern engine, retrieval, payload builder, and UI read.
//!
//! At-rest encryption (SQLCipher-style page encryption, key wrapped by Windows
//! DPAPI — doc 13 §6) is applied when the connection is opened; the key is
//! supplied by `aperture-privacy::key_manager`. Loss of the key => DB
//! unreadable, **by design**.
//!
//! ## Concurrency model (doc 03 §1)
//! Single-writer: one [`Db`] handle wraps one `rusqlite::Connection` behind a
//! `Mutex`. The Tier-0 pipeline owns the writes; readers clone the `Arc<Db>`
//! and take short read locks. WAL mode keeps readers unblocked by the writer.

pub mod migrations;
pub mod retention;

use std::path::PathBuf;
use std::sync::Mutex;

use aperture_contracts::{Event, EventType};
use rusqlite::Connection;

/// `%LOCALAPPDATA%\Aperture\history.db` (doc 03 §1).
///
/// [VERIFY resolved — Step 0]: `%LOCALAPPDATA%` is read from the environment
/// (always set in Windows sessions); the fallback (`.`/dev) only applies in
/// test harnesses without a profile.
pub fn default_db_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Aperture").join("history.db")
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("the database could not be decrypted (wrong or missing key)")]
    Decryption,
    #[error("io error: {0}")]
    Io(String),
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e.to_string())
    }
}

/// A `screen_context` row to persist alongside its event (doc 03 §3). Mirrors
/// `aperture-vision-ocr`'s row shape without depending on that crate (the DB
/// sits below the vision pipeline in the dependency order).
#[derive(Debug, Clone, Default)]
pub struct ScreenContextInsert {
    pub ocr_text: Option<String>,
    pub ocr_confidence: Option<f64>,
    pub vlm_summary: Option<String>,
    pub thumb_phash: Option<String>,
}

/// One KNN hit from the doc 03 §5 retrieval query.
#[derive(Debug, Clone)]
pub struct KnnHit {
    pub event_id: i64,
    pub ts: i64,
    pub event_type: String,
    pub window_title: Option<String>,
    pub payload: serde_json::Value,
    pub connector_type: Option<String>,
    pub reconstruct_payload: Option<serde_json::Value>,
    /// The event's `connector_id` (the Resume-action ref for the voice answer
    /// bubble, doc 07 §5); `None` when the event has no connector.
    pub connector_id: Option<String>,
    /// The joined connector's TTL (doc 10). `Some(ts)` past which it is stale;
    /// `None` = never stale. Lets retrieval offer Resume only on a *fresh* state.
    pub stale_after_ts: Option<i64>,
    pub distance: f64,
}

/// Whether this build can actually encrypt at rest (doc 13 §6).
///
/// `true` only when the `sqlcipher` feature is on, which swaps rusqlite's plain
/// `bundled` SQLite for `bundled-sqlcipher-vendored-openssl`. That build needs a
/// **native Windows Perl + NASM** on PATH to compile vendored OpenSSL — see the
/// crate README note. With the feature off the DB is plaintext on disk and
/// [`Db::is_encrypted`] reports `false`, so the M9 gate fails loudly instead of a
/// silent downgrade (never claim encryption we did not apply).
pub const ENCRYPTION_AVAILABLE: bool = cfg!(feature = "sqlcipher");

/// A handle to the history DB. Wraps a `rusqlite::Connection` plus the loaded
/// sqlite-vec extension. Cheap to share as `Arc<Db>`.
pub struct Db {
    conn: Mutex<Connection>,
    /// Whether the sqlite-vec extension registered — `ctx_vec` ops require it.
    /// (`false` only in stripped-down builds; the M2 gate asserts `true`.)
    vec_loaded: bool,
    /// Whether page encryption is actually in force on this handle (doc 13 §6):
    /// the `sqlcipher` feature is on AND a non-empty key was applied.
    encrypted: bool,
}

impl Db {
    /// Open (creating if needed) the history DB, load sqlite-vec, set WAL, and
    /// run pending migrations. `key` is the **unwrapped** page key from
    /// `aperture_privacy::key_manager` (doc 13 §6) — DPAPI unwrapping happens
    /// there; this layer only applies it.
    ///
    /// M9: when the `sqlcipher` feature is on, `PRAGMA key` is applied as the
    /// FIRST statement on the connection (before WAL/foreign_keys/migrations —
    /// SQLCipher requires the key before any other access), then the key is
    /// **verified** by forcing a read of the schema. A wrong/missing key on an
    /// existing encrypted file surfaces [`DbError::Decryption`]: the DB is
    /// unreadable by design (doc 13 §6, "key loss ⇒ unreadable").
    pub fn open_encrypted(path: PathBuf, key: &[u8]) -> Result<Self, DbError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| DbError::Io(e.to_string()))?;
        }
        // Register sqlite-vec as an auto-extension BEFORE opening, so the
        // migration's `CREATE VIRTUAL TABLE ... USING vec0` works (doc 03 §3).
        let vec_loaded = register_sqlite_vec();
        let conn = Connection::open(&path)?;
        // MUST precede every other statement on this connection (doc 13 §6).
        let encrypted = apply_key(&conn, key)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Overwrite freed pages instead of leaving their content in the file
        // (doc 13 §7). Without this, deleted OCR text lingers in free pages
        // between a delete and the next VACUUM — including the nightly pruner's
        // deletes, which never VACUUM at all. Costs some write amplification;
        // for a privacy product that is the right trade.
        conn.pragma_update(None, "secure_delete", "ON")?;
        migrations::run(&conn, vec_loaded)?;
        Ok(Self { conn: Mutex::new(conn), vec_loaded, encrypted })
    }

    /// Open an in-memory DB (tests / gates). Same migrations, no file, no key.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let vec_loaded = register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::run(&conn, vec_loaded)?;
        Ok(Self { conn: Mutex::new(conn), vec_loaded, encrypted: false })
    }

    /// Whether `ctx_vec` (sqlite-vec) is available on this handle.
    pub fn vec_available(&self) -> bool {
        self.vec_loaded
    }

    /// Whether page encryption is actually in force (doc 13 §6). The M9 gate
    /// asserts this; the shell logs a prominent warning when it is `false` so a
    /// plaintext DB is never mistaken for an encrypted one.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// Run one closure against the raw connection (short lock; doc 03 §1 —
    /// readers use this for SELECTs, the single writer for its transactions).
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    ) -> Result<T, DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        f(&conn).map_err(DbError::from)
    }

    // ---------------------------------------------------------------------
    // Typed Tier-0 write path (doc 03 §1-§3; single-writer)
    // ---------------------------------------------------------------------

    /// Insert one [`Event`]; returns the DB-assigned id (doc 15 §1: `id` is 0 on
    /// the bus, assigned here).
    pub fn insert_event(&self, ev: &Event) -> Result<i64, DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        insert_event_tx(&conn, ev)?;
        Ok(conn.last_insert_rowid())
    }

    /// The M2 store step (doc 02 §4 step 5, doc 16 M2): write the event, its
    /// `screen_context` row, and its 768-d embedding into `ctx_vec` in **one
    /// transaction** — the embedding write and the event row are atomic (build
    /// prompt: "sqlite-vec write must be in the same transaction").
    ///
    /// `embedding` is optional (empty OCR text embeds nothing, doc 06 §6);
    /// `ctx` is optional for events with no sampled frame.
    pub fn insert_event_with_context(
        &self,
        ev: &Event,
        ctx: Option<&ScreenContextInsert>,
        embedding: Option<&[f32]>,
    ) -> Result<i64, DbError> {
        let mut guard = self.conn.lock().expect("db mutex poisoned");
        let tx = guard.transaction().map_err(DbError::from)?;

        insert_event_tx(&tx, ev)?;
        let event_id = tx.last_insert_rowid();

        if let Some(c) = ctx {
            tx.execute(
                "INSERT INTO screen_context (event_id, ocr_text, ocr_confidence, vlm_summary, thumb_phash) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![event_id, c.ocr_text, c.ocr_confidence, c.vlm_summary, c.thumb_phash],
            )
            .map_err(DbError::from)?;
        }

        if let Some(vec) = embedding {
            if !self.vec_loaded {
                return Err(DbError::Sqlite(
                    "ctx_vec write requested but sqlite-vec is not loaded".into(),
                ));
            }
            debug_assert_eq!(vec.len(), 768, "ctx_vec is pinned to 768-d (doc 03 §3)");
            tx.execute(
                "INSERT INTO ctx_vec (event_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![event_id, vec_to_blob(vec)],
            )
            .map_err(DbError::from)?;
        }

        tx.commit().map_err(DbError::from)?;
        Ok(event_id)
    }

    /// Attach a `screen_context` row (+ optional 768-d embedding) to an event
    /// that was already persisted (the trigger-sampled path: the event row was
    /// written persist-then-notify, the frame's OCR finished afterwards). One
    /// transaction, mirroring [`Self::insert_event_with_context`].
    pub fn attach_context(
        &self,
        event_id: i64,
        ctx: &ScreenContextInsert,
        embedding: Option<&[f32]>,
    ) -> Result<(), DbError> {
        let mut guard = self.conn.lock().expect("db mutex poisoned");
        let tx = guard.transaction().map_err(DbError::from)?;
        tx.execute(
            "INSERT INTO screen_context (event_id, ocr_text, ocr_confidence, vlm_summary, thumb_phash) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![event_id, ctx.ocr_text, ctx.ocr_confidence, ctx.vlm_summary, ctx.thumb_phash],
        )
        .map_err(DbError::from)?;
        if let Some(vec) = embedding {
            if !self.vec_loaded {
                return Err(DbError::Sqlite(
                    "ctx_vec write requested but sqlite-vec is not loaded".into(),
                ));
            }
            debug_assert_eq!(vec.len(), 768, "ctx_vec is pinned to 768-d (doc 03 §3)");
            tx.execute(
                "INSERT INTO ctx_vec (event_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![event_id, vec_to_blob(vec)],
            )
            .map_err(DbError::from)?;
        }
        tx.commit().map_err(DbError::from)
    }

    /// Attach the VLM scene summary to an already-stored frame's `screen_context`
    /// (doc 06 §5, Layer B): the enrichment lands on the row the OCR wrote, and
    /// improves the *next* pattern cycle — it never gates the current bubble
    /// (doc 02 Path A). Returns `false` if the row is gone (pruned / no context).
    pub fn attach_vlm_summary(&self, event_id: i64, summary: &str) -> Result<bool, DbError> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE screen_context SET vlm_summary = ?2 WHERE event_id = ?1",
                rusqlite::params![event_id, summary],
            )
            .map(|n| n > 0)
        })
    }

    /// Read one event back by id (round-trip surface for the M0 gate).
    pub fn read_event(&self, id: i64) -> Result<Event, DbError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, ts, type, app, process, window_title, payload, \
                        connector_id, session_id, redaction_flags \
                 FROM events WHERE id = ?1",
                [id],
                row_to_event,
            )
        })
    }

    // ---------------------------------------------------------------------
    // Connector state (doc 03 §3, doc 10 §1) — the M4 resumable-handle store
    // ---------------------------------------------------------------------

    /// Insert one `connector_state` row (Path A step 4, doc 02 §4). The id is
    /// connector-assigned (uuid) — never generated here.
    pub fn insert_connector_state(
        &self,
        st: &aperture_contracts::ConnectorState,
    ) -> Result<(), DbError> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO connector_state \
                 (id, connector_type, reconstruct_payload, payload_version, captured_ts, stale_after_ts) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    st.id,
                    st.connector_type,
                    st.reconstruct_payload.to_string(),
                    st.payload_version,
                    st.captured_ts,
                    st.stale_after_ts,
                ],
            )
            .map(|_| ())
        })
    }

    /// Refresh an existing row in place (the pipeline's coalescing path: same
    /// resource re-observed ⇒ freshest payload/position wins, doc 10 §3).
    /// Returns `false` when the row no longer exists (pruned) — insert instead.
    pub fn refresh_connector_state(
        &self,
        id: &str,
        reconstruct_payload: &serde_json::Value,
        captured_ts: i64,
        stale_after_ts: Option<i64>,
    ) -> Result<bool, DbError> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE connector_state SET reconstruct_payload = ?2, captured_ts = ?3, stale_after_ts = ?4 \
                 WHERE id = ?1",
                rusqlite::params![id, reconstruct_payload.to_string(), captured_ts, stale_after_ts],
            )
            .map(|n| n > 0)
        })
    }

    /// Load one `connector_state` row (Path B step 2, doc 02 §5).
    pub fn read_connector_state(
        &self,
        id: &str,
    ) -> Result<Option<aperture_contracts::ConnectorState>, DbError> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT id, connector_type, reconstruct_payload, payload_version, captured_ts, stale_after_ts \
                 FROM connector_state WHERE id = ?1",
                [id],
                row_to_connector_state,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })
    }

    /// The freshest non-stale states of one type, newest first (pattern-engine
    /// trigger rule 3, doc 08 §6: "a fresh, resumable connector_state exists
    /// for the consequent"). `limit` small — the caller may post-filter (e.g.
    /// match a `doc:xlsx` token to a `.xlsx` payload path).
    pub fn fresh_connector_states(
        &self,
        connector_type: &str,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<aperture_contracts::ConnectorState>, DbError> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, connector_type, reconstruct_payload, payload_version, captured_ts, stale_after_ts \
                 FROM connector_state \
                 WHERE connector_type = ?1 AND (stale_after_ts IS NULL OR stale_after_ts > ?2) \
                 ORDER BY captured_ts DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![connector_type, now_ms, limit],
                row_to_connector_state,
            )?;
            rows.collect()
        })
    }

    /// Stamp `events.connector_id` after a capture (doc 03 §3: the event row
    /// references the resumable handle it produced).
    pub fn set_event_connector(&self, event_id: i64, connector_id: &str) -> Result<(), DbError> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE events SET connector_id = ?2 WHERE id = ?1",
                rusqlite::params![event_id, connector_id],
            )
            .map(|_| ())
        })
    }

    // ---------------------------------------------------------------------
    // Retrieval (doc 03 §5) — KNN + join + filter
    // ---------------------------------------------------------------------

    /// The doc 03 §5 KNN retrieval: nearest `k` stored contexts to `query_vec`,
    /// joined to their events + connector state, filtered by `recency_floor_ms`
    /// (epoch ms; pass 0 for no floor), ordered by ascending distance.
    ///
    /// Also invoked by the gated `aperture_search_history` MCP tool at M7
    /// (ADR-037) — same SQL, results redacted + previewed before return.
    pub fn knn(
        &self,
        query_vec: &[f32],
        k: u32,
        recency_floor_ms: i64,
    ) -> Result<Vec<KnnHit>, DbError> {
        if !self.vec_loaded {
            return Err(DbError::Sqlite("sqlite-vec is not loaded".into()));
        }
        // sqlite-vec's `MATCH … AND k = N` truncates to the N globally-nearest
        // BEFORE the `e.ts >= floor` join filter, so a time-scoped query could lose
        // most/all of its k to out-of-window neighbours (a recall cliff). Oversample
        // the vector search, then apply the floor and LIMIT to the requested k — the
        // returned k are the nearest *within* the window (doc 03 §5).
        let oversample = k.saturating_mul(8).clamp(k, 512);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "WITH knn AS ( \
                   SELECT event_id, distance FROM ctx_vec \
                   WHERE embedding MATCH ?1 AND k = ?2 \
                 ) \
                 SELECT e.id, e.ts, e.type, e.window_title, e.payload, \
                        cs.connector_type, cs.reconstruct_payload, knn.distance, \
                        e.connector_id, cs.stale_after_ts \
                 FROM knn \
                 JOIN events e ON e.id = knn.event_id \
                 LEFT JOIN connector_state cs ON cs.id = e.connector_id \
                 WHERE e.ts >= ?3 \
                 ORDER BY knn.distance ASC \
                 LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![vec_to_blob(query_vec), oversample, recency_floor_ms, k],
                |row| {
                    let payload_text: String = row.get(4)?;
                    let reconstruct_text: Option<String> = row.get(6)?;
                    Ok(KnnHit {
                        event_id: row.get(0)?,
                        ts: row.get(1)?,
                        event_type: row.get(2)?,
                        window_title: row.get(3)?,
                        payload: serde_json::from_str(&payload_text)
                            .unwrap_or(serde_json::Value::Null),
                        connector_type: row.get(5)?,
                        reconstruct_payload: reconstruct_text
                            .and_then(|t| serde_json::from_str(&t).ok()),
                        connector_id: row.get(8)?,
                        stale_after_ts: row.get(9)?,
                        distance: row.get(7)?,
                    })
                },
            )?;
            rows.collect()
        })
    }

    /// One-click Purge All: wipe the history + VACUUM (doc 03 §6, doc 13 §7).
    ///
    /// Audit rows (`capture_toggle`, `cloud_send`) **survive** if they are within
    /// `policy.audit_days` of `now_ms`; older audit rows go with everything else,
    /// so the 30-day accountability window is honored exactly (doc 13 §7).
    ///
    /// **Amendment to doc 03 §6's "truncates every table" (M9).** Three tables
    /// are deliberately preserved:
    /// - `exclusion_list` — purging it would *silently reduce* the user's privacy
    ///   protection: apps they had excluded would start being captured again on
    ///   the next frame. A privacy control must never weaken itself.
    /// - `settings` — holds consent (doc 13 §8). Wiping it would re-trigger the
    ///   first-run flow and reset capture consent as a side effect of a *data*
    ///   purge, which the user did not ask for.
    /// - `schema_migrations` — dropping it would re-run migrations over live tables.
    ///
    /// Everything else (events + their `screen_context`/`ctx_vec`, patterns,
    /// suggestions, connector state) is deleted. VACUUM **followed by a
    /// `wal_checkpoint(TRUNCATE)`** then makes the purge real on disk rather than
    /// only logical — see the comment at the call site for why the checkpoint is
    /// load-bearing in WAL mode.
    ///
    /// Returns the number of rows deleted from `events` (what the confirmation
    /// UX reports back).
    pub fn purge_all(&self, now_ms: i64, policy: &retention::RetentionPolicy) -> Result<usize, DbError> {
        let audit_floor = now_ms - policy.audit_days as i64 * 86_400_000;

        let deleted = self.with_conn(|conn| {
            conn.execute_batch("BEGIN")?;
            // Same discipline as retention::run_nightly_prune: never leave BEGIN
            // open on the shared connection, or every later write silently joins
            // a dead transaction.
            let purged = (|| -> Result<usize, rusqlite::Error> {
                // ctx_vec is a vec0 virtual table — no FK cascade reaches it, so
                // it is cleared explicitly and FIRST (tolerate its absence when
                // sqlite-vec did not load).
                let _ = conn.execute(
                    "DELETE FROM ctx_vec WHERE event_id IN \
                     (SELECT id FROM events WHERE type NOT IN ('capture_toggle','cloud_send') OR ts < ?1)",
                    [audit_floor],
                );
                // Events: everything except audit rows inside the survival window.
                // `screen_context` cascades (real table, FK ON DELETE CASCADE).
                let events_deleted = conn.execute(
                    "DELETE FROM events \
                     WHERE type NOT IN ('capture_toggle','cloud_send') OR ts < ?1",
                    [audit_floor],
                )?;
                conn.execute_batch(
                    "DELETE FROM suggestions; \
                     DELETE FROM patterns; \
                     DELETE FROM connector_state;",
                )?;
                Ok(events_deleted)
            })();

            match purged {
                Ok(n) => {
                    conn.execute_batch("COMMIT")?;
                    Ok(n)
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })?;

        // VACUUM cannot run inside a transaction, so it follows the COMMIT.
        //
        // ...and VACUUM ALONE IS NOT ENOUGH IN WAL MODE. We hold this connection
        // open for the whole process lifetime, so VACUUM writes the rebuilt
        // database into `history.db-wal` while `history.db` keeps its pre-purge
        // pages until something checkpoints. Without the truncate below, every
        // "purged" row stayed verbatim-recoverable from the two files on disk —
        // the exact hand-over-the-laptop case Purge All exists for, and plaintext
        // in the default (non-`sqlcipher`) build. `TRUNCATE` checkpoints the WAL
        // into the main DB and then zeroes the `-wal` file.
        self.with_conn(|conn| {
            conn.execute_batch("VACUUM")?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        })?;
        tracing::info!(events_deleted = deleted, "purge all complete (doc 13 §7)");
        Ok(deleted)
    }

    /// Recent audit rows — `capture_toggle` + `cloud_send`, newest first
    /// (doc 13 §3). Backs the Activity & Privacy view's two questions: "when was
    /// it watching?" and "what ever left this machine?".
    pub fn recent_audit_events(&self, limit: u32) -> Result<Vec<Event>, DbError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, ts, type, app, process, window_title, payload, connector_id, \
                        session_id, redaction_flags \
                 FROM events WHERE type IN ('capture_toggle','cloud_send') \
                 ORDER BY ts DESC, id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], row_to_event)?;
            rows.collect()
        })
    }

    // ---------------------------------------------------------------------
    // Exclusion list (doc 13 §4) — the persisted half of `capture::exclusion`.
    // ---------------------------------------------------------------------

    /// Read every exclusion row as `(id, match_kind, pattern, enabled)`
    /// (doc 13 §4). The shell compiles the enabled ones into a
    /// `capture::exclusion::ExclusionList`; this crate sits below capture, so it
    /// deliberately returns raw tuples rather than depending upward.
    pub fn read_exclusion_list(&self) -> Result<Vec<(i64, String, String, bool)>, DbError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, match_kind, pattern, enabled FROM exclusion_list ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                ))
            })?;
            rows.collect()
        })
    }

    /// Add one exclusion rule — the "exclude this app/domain" affordance
    /// (doc 13 §9). Idempotent on `(match_kind, pattern)` so clicking twice does
    /// not accumulate duplicates. Returns the row id.
    pub fn add_exclusion_rule(&self, match_kind: &str, pattern: &str) -> Result<i64, DbError> {
        self.with_conn(|conn| {
            if let Ok(id) = conn.query_row(
                "SELECT id FROM exclusion_list WHERE match_kind = ?1 AND pattern = ?2",
                rusqlite::params![match_kind, pattern],
                |r| r.get::<_, i64>(0),
            ) {
                conn.execute("UPDATE exclusion_list SET enabled = 1 WHERE id = ?1", [id])?;
                return Ok(id);
            }
            conn.execute(
                "INSERT INTO exclusion_list (match_kind, pattern, enabled) VALUES (?1, ?2, 1)",
                rusqlite::params![match_kind, pattern],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    /// Enable/disable or delete one exclusion rule from the Activity & Privacy
    /// view (doc 13 §7). `enabled = None` deletes the row.
    pub fn set_exclusion_enabled(&self, id: i64, enabled: Option<bool>) -> Result<(), DbError> {
        self.with_conn(|conn| {
            match enabled {
                None => conn.execute("DELETE FROM exclusion_list WHERE id = ?1", [id])?,
                Some(on) => conn.execute(
                    "UPDATE exclusion_list SET enabled = ?2 WHERE id = ?1",
                    rusqlite::params![id, i64::from(on)],
                )?,
            };
            Ok(())
        })
    }

    // ---------------------------------------------------------------------
    // Settings (doc 13 §6) — the encrypted key/value store consent lives in.
    // ---------------------------------------------------------------------

    /// Read one `settings` value as raw JSON text; `None` when absent.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        self.with_conn(|conn| {
            conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
        })
    }

    /// Upsert one `settings` value (raw JSON text).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
    }
}

/// Apply the SQLCipher page key, if this build has encryption compiled in.
/// Returns whether encryption is actually in force. MUST be called before any
/// other statement on the connection (doc 13 §6).
#[cfg(feature = "sqlcipher")]
fn apply_key(conn: &Connection, key: &[u8]) -> Result<bool, DbError> {
    if key.is_empty() {
        // An empty key with encryption compiled in is a composition bug, not a
        // downgrade path — refuse rather than silently writing plaintext.
        return Err(DbError::Io(
            "sqlcipher build requires a non-empty page key (doc 13 §6)".into(),
        ));
    }
    // Raw-key form `PRAGMA key = "x'<hex>'"` uses the bytes verbatim (no KDF
    // over a passphrase) — the key already came from a CSPRNG. `hex` is our own
    // generated alphabet, so the format! is not an injection surface.
    let mut hex = String::with_capacity(key.len() * 2);
    for byte in key {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
        .map_err(|_| DbError::Decryption)?;
    // Force a real read: with a wrong key SQLCipher fails here, not at PRAGMA.
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .map_err(|_| DbError::Decryption)?;
    Ok(true)
}

/// No-encryption build: the key is accepted (so every caller is already
/// M9-shaped) but NOT applied. Loud about it — a plaintext DB must never be
/// mistaken for an encrypted one (doc 13 §6).
#[cfg(not(feature = "sqlcipher"))]
fn apply_key(_conn: &Connection, key: &[u8]) -> Result<bool, DbError> {
    if !key.is_empty() {
        tracing::warn!(
            "at-rest encryption is NOT compiled in (cargo feature `sqlcipher` off): \
             the history DB is PLAINTEXT on disk. Doc 13 §6 requires the encrypted \
             build for the M9 gate."
        );
    }
    Ok(false)
}

/// INSERT one event (shared by the plain and transactional paths).
fn insert_event_tx(conn: &Connection, ev: &Event) -> Result<(), DbError> {
    let type_str = event_type_str(ev.r#type);
    let payload_text =
        serde_json::to_string(&ev.payload).map_err(|e| DbError::Sqlite(e.to_string()))?;
    conn.execute(
        "INSERT INTO events \
         (ts, type, app, process, window_title, payload, connector_id, session_id, redaction_flags) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            ev.ts,
            type_str,
            ev.app,
            ev.process,
            ev.window_title,
            payload_text,
            ev.connector_id,
            ev.session_id,
            ev.redaction_flags,
        ],
    )
    .map_err(DbError::from)?;
    Ok(())
}

/// Map a DB row (in `read_event` column order) back into a typed [`Event`].
fn row_to_connector_state(
    row: &rusqlite::Row<'_>,
) -> Result<aperture_contracts::ConnectorState, rusqlite::Error> {
    let payload_text: String = row.get(2)?;
    Ok(aperture_contracts::ConnectorState {
        id: row.get(0)?,
        connector_type: row.get(1)?,
        reconstruct_payload: serde_json::from_str(&payload_text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        payload_version: row.get(3)?,
        captured_ts: row.get(4)?,
        stale_after_ts: row.get(5)?,
    })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> Result<Event, rusqlite::Error> {
    let type_text: String = row.get(2)?;
    let payload_text: String = row.get(6)?;
    Ok(Event {
        id: row.get(0)?,
        ts: row.get(1)?,
        r#type: serde_json::from_value(serde_json::Value::String(type_text)).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        app: row.get(3)?,
        process: row.get(4)?,
        window_title: row.get(5)?,
        payload: serde_json::from_str(&payload_text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?,
        connector_id: row.get(7)?,
        session_id: row.get(8)?,
        redaction_flags: row.get(9)?,
    })
}

/// The snake_case wire string for an [`EventType`] — the same string `serde`
/// produces, so the `type` column and the bus message agree (doc 15 §1).
pub fn event_type_str(ty: EventType) -> String {
    serde_json::to_value(ty)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("EventType serializes to a string")
}

/// Encode an f32 slice as the little-endian blob sqlite-vec expects for a
/// `float[N]` column.
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Register sqlite-vec as an auto-extension for every subsequently opened
/// connection. Returns `false` (and logs) if registration fails — callers then
/// operate without `ctx_vec` (the M2 gate asserts it loaded).
fn register_sqlite_vec() -> bool {
    use std::sync::OnceLock;
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        // The entry-point signature sqlite3_auto_extension expects (xEntryPoint).
        type SqliteExtInit = unsafe extern "C" fn(
            *mut libsqlite3_sys::sqlite3,
            *mut *const i8,
            *const libsqlite3_sys::sqlite3_api_routines,
        ) -> i32;
        // SAFETY: sqlite3_vec_init is the extension's documented entry point with
        // exactly the xEntryPoint ABI (it is compiled against the same bundled
        // SQLite via libsqlite3-sys); sqlite3_auto_extension is the documented
        // way to register it process-wide before connections open (sqlite-vec
        // crate README). The transmute only unifies the two crates' identical
        // repr(C) pointer types.
        unsafe {
            let rc = libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteExtInit,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
            if rc == libsqlite3_sys::SQLITE_OK {
                true
            } else {
                tracing::error!(rc, "sqlite-vec auto-extension registration failed");
                false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(ty: EventType) -> Event {
        Event {
            id: 0,
            ts: 1_700_000_000_000,
            r#type: ty,
            app: Some("TestApp".into()),
            process: Some("test.exe".into()),
            window_title: Some("t".into()),
            payload: serde_json::json!({"k": 1}),
            connector_id: None,
            session_id: Some(1),
            redaction_flags: 0,
        }
    }

    #[test]
    fn event_roundtrip_via_typed_api() {
        let db = Db::open_in_memory().expect("open");
        let ev = sample_event(EventType::WindowFocus);
        let id = db.insert_event(&ev).expect("insert");
        let got = db.read_event(id).expect("read");
        assert_eq!(got.r#type, ev.r#type);
        assert_eq!(got.payload, ev.payload);
        assert!(got.id > 0);
    }

    #[test]
    fn atomic_event_context_embedding_write_then_knn() {
        let db = Db::open_in_memory().expect("open");
        assert!(db.vec_available(), "sqlite-vec must load (M2 gate)");

        // Two contexts with distinct embeddings.
        let mut e1 = vec![0.0f32; 768];
        e1[0] = 1.0;
        let mut e2 = vec![0.0f32; 768];
        e2[1] = 1.0;

        let ctx = ScreenContextInsert {
            ocr_text: Some("budget spreadsheet quarterly numbers".into()),
            ocr_confidence: Some(0.9),
            ..Default::default()
        };
        let id1 = db
            .insert_event_with_context(&sample_event(EventType::WindowFocus), Some(&ctx), Some(&e1))
            .expect("insert 1");
        let _id2 = db
            .insert_event_with_context(&sample_event(EventType::Navigation), Some(&ctx), Some(&e2))
            .expect("insert 2");

        // A query vector near e1 must rank id1 first (doc 16 M2: sane KNN).
        let mut q = vec![0.0f32; 768];
        q[0] = 0.9;
        let hits = db.knn(&q, 2, 0).expect("knn");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].event_id, id1, "nearest neighbour is the matching event");
        assert!(hits[0].distance <= hits[1].distance);
    }

    #[test]
    fn knn_recency_floor_does_not_starve_in_window_hits() {
        // Regression (review #7): the recency floor must not be applied only AFTER
        // vec-truncation, or out-of-window near-neighbours crowd out the in-window
        // hit. Two OLD events sit exactly on the query (distance 0); one RECENT
        // event is slightly farther. With k=2 and a floor between them, the naive
        // "k-nearest then filter" would return nothing; oversampling must surface
        // the in-window hit.
        let db = Db::open_in_memory().expect("open");
        let at = |ts: i64| {
            let mut e = sample_event(EventType::WindowFocus);
            e.ts = ts;
            e
        };
        let mut on_query = vec![0.0f32; 768];
        on_query[0] = 1.0; // distance 0 to the query
        let mut slightly_off = vec![0.0f32; 768];
        slightly_off[0] = 0.8;
        slightly_off[1] = 0.6; // unit, but farther from the query

        // Two nearest-but-OLD, one farther-but-RECENT.
        db.insert_event_with_context(&at(1_000), None, Some(&on_query)).unwrap();
        db.insert_event_with_context(&at(2_000), None, Some(&on_query)).unwrap();
        let recent_id = db
            .insert_event_with_context(&at(9_000_000), None, Some(&slightly_off))
            .unwrap();

        let mut q = vec![0.0f32; 768];
        q[0] = 1.0;
        let hits = db.knn(&q, 2, 5_000_000).expect("knn");
        assert!(
            hits.iter().any(|h| h.event_id == recent_id),
            "the in-window hit must survive the recency floor (got {} hits)",
            hits.len()
        );
        assert!(
            hits.iter().all(|h| h.ts >= 5_000_000),
            "no out-of-window (old) event may be returned"
        );
    }

    // -----------------------------------------------------------------------
    // M9 — Purge All (doc 13 §7)
    // -----------------------------------------------------------------------

    const DAY_MS: i64 = 86_400_000;

    fn ev_at(ts: i64, ty: EventType) -> Event {
        Event { ts, ..sample_event(ty) }
    }

    #[test]
    fn purge_all_wipes_history_but_keeps_audit_rows_inside_the_window() {
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000 + 100 * DAY_MS;
        let policy = retention::RetentionPolicy::default(); // audit_days = 30

        let history = db.insert_event(&ev_at(now - DAY_MS, EventType::WindowFocus)).unwrap();
        let fresh_audit = db.insert_event(&ev_at(now - DAY_MS, EventType::CloudSend)).unwrap();
        let stale_audit = db.insert_event(&ev_at(now - 60 * DAY_MS, EventType::CaptureToggle)).unwrap();

        let deleted = db.purge_all(now, &policy).expect("purge");

        assert!(db.read_event(history).is_err(), "history is purged");
        assert!(db.read_event(fresh_audit).is_ok(), "audit inside 30 d survives (doc 13 §7)");
        assert!(db.read_event(stale_audit).is_err(), "audit past 30 d expires with the rest");
        assert_eq!(deleted, 2, "one history row + one expired audit row");
    }

    #[test]
    fn purge_all_preserves_settings_and_exclusions_m9_amendment() {
        // A privacy control must never weaken itself: purging the exclusion list
        // would silently resume capturing apps the user had excluded.
        let db = Db::open_in_memory().expect("open");
        db.set_setting("consent", r#"{"capture_enabled":true}"#).unwrap();
        let rule = db.add_exclusion_rule("process", "1password.exe").unwrap();

        db.purge_all(1_700_000_000_000, &retention::RetentionPolicy::default()).unwrap();

        assert_eq!(
            db.get_setting("consent").unwrap().as_deref(),
            Some(r#"{"capture_enabled":true}"#),
            "consent survives a data purge"
        );
        let rules = db.read_exclusion_list().unwrap();
        assert!(rules.iter().any(|(id, ..)| *id == rule), "exclusions survive a data purge");
    }

    #[test]
    fn purge_all_leaves_the_connection_usable() {
        let db = Db::open_in_memory().expect("open");
        let now = 1_700_000_000_000;
        db.insert_event(&ev_at(now, EventType::WindowFocus)).unwrap();
        db.purge_all(now, &retention::RetentionPolicy::default()).unwrap();
        // VACUUM outside the transaction must not leave BEGIN dangling.
        let id = db.insert_event(&ev_at(now, EventType::WindowFocus)).expect("writable after purge");
        assert!(db.read_event(id).is_ok());
    }

    #[test]
    fn exclusion_rules_upsert_idempotently_and_toggle() {
        let db = Db::open_in_memory().expect("open");
        let a = db.add_exclusion_rule("process", "1password.exe").unwrap();
        let b = db.add_exclusion_rule("process", "1password.exe").unwrap();
        assert_eq!(a, b, "clicking `exclude this app` twice must not duplicate the rule");
        assert_eq!(db.read_exclusion_list().unwrap().len(), 1);

        db.set_exclusion_enabled(a, Some(false)).unwrap();
        assert!(!db.read_exclusion_list().unwrap()[0].3, "rule disabled");

        // Re-adding a disabled rule re-enables it rather than inserting a twin.
        db.add_exclusion_rule("process", "1password.exe").unwrap();
        assert!(db.read_exclusion_list().unwrap()[0].3, "re-add re-enables");

        db.set_exclusion_enabled(a, None).unwrap();
        assert!(db.read_exclusion_list().unwrap().is_empty(), "None deletes");
    }

    #[test]
    fn settings_roundtrip_and_miss_is_none() {
        let db = Db::open_in_memory().expect("open");
        assert_eq!(db.get_setting("absent").unwrap(), None);
        db.set_setting("k", "\"v\"").unwrap();
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("\"v\""));
        db.set_setting("k", "\"v2\"").unwrap();
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("\"v2\""), "upsert overwrites");
    }

    #[test]
    fn in_memory_db_reports_itself_unencrypted() {
        // The gate relies on this being truthful, never optimistic.
        let db = Db::open_in_memory().expect("open");
        assert!(!db.is_encrypted());
    }
}
