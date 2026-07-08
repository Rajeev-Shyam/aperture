//! Forward-only schema migrations (doc 03 §3, doc 16 M0 gate).
//!
//! The M0 validation gate requires that the schema round-trips **every**
//! [`aperture_contracts::EventType`]; see `crates/gates/tests`.

use rusqlite::Connection;

use crate::DbError;

/// Migrations embedded at compile time, applied in order. `schema_migrations`
/// records the highest applied `version`.
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_pattern_mute_persist.sql")),
];

/// Apply every migration whose version is newer than `schema_migrations.MAX(version)`.
///
/// Each migration runs inside one transaction and its version row is recorded in
/// the same transaction, so a failed migration leaves the DB at the previous
/// version (forward-only, doc 03 §3).
///
/// `vec_loaded` — whether the sqlite-vec extension registered. When it did not
/// (stripped-down test builds), the single `CREATE VIRTUAL TABLE … USING vec0`
/// statement is skipped so the plain tables still migrate; everything else MUST
/// succeed. The M2 gate asserts `vec_loaded == true` in the product.
pub fn run(conn: &Connection, vec_loaded: bool) -> Result<(), DbError> {
    // Bootstrap: schema_migrations must exist to know where we are.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_ts INTEGER);",
    )
    .map_err(|e| DbError::Migration(e.to_string()))?;

    let applied: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))
        .map_err(|e| DbError::Migration(e.to_string()))?;

    for (version, sql) in pending_after(applied) {
        conn.execute_batch("BEGIN")
            .map_err(|e| DbError::Migration(e.to_string()))?;
        let result = apply_one(conn, *version, sql, vec_loaded);
        match result {
            Ok(()) => {
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_ts) VALUES (?1, ?2)",
                    rusqlite::params![version, now_ms()],
                )
                .map_err(|e| DbError::Migration(e.to_string()))?;
                conn.execute_batch("COMMIT")
                    .map_err(|e| DbError::Migration(e.to_string()))?;
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Apply one migration's statements. Statement-by-statement so the `vec0`
/// virtual table can be skipped when the extension is unavailable (see [`run`]).
fn apply_one(conn: &Connection, version: i64, sql: &str, vec_loaded: bool) -> Result<(), DbError> {
    for stmt in split_sql_statements(sql) {
        let is_vec0 = stmt.to_ascii_lowercase().contains("using vec0");
        if is_vec0 && !vec_loaded {
            tracing::warn!(version, "skipping vec0 table (sqlite-vec not loaded)");
            continue;
        }
        // `CREATE TABLE schema_migrations` re-runs harmlessly guarded below.
        if stmt.to_ascii_lowercase().contains("create table schema_migrations") {
            continue; // bootstrapped in `run`
        }
        conn.execute_batch(&format!("{stmt};")).map_err(|e| {
            DbError::Migration(format!("v{version}: `{stmt}`: {e}"))
        })?;
    }
    Ok(())
}

/// Migrations newer than `applied_version`, in order.
pub fn pending_after(applied_version: i64) -> impl Iterator<Item = &'static (i64, &'static str)> {
    MIGRATIONS.iter().filter(move |(v, _)| *v > applied_version)
}

/// `;`-splitter for the embedded DDL. `--` line comments are stripped FIRST so a
/// `;` inside a comment (e.g. "perceptual hash; RAW FRAMES ARE NOT STORED") can
/// never truncate a statement. The init schema (doc 03 §3) has no `;` inside
/// string literals or trigger bodies, so the literal split on the comment-free
/// text is correct. (If a future migration adds a trigger body with embedded
/// `;`, load sqlite-vec unconditionally and `execute_batch` the whole file.)
fn split_sql_statements(sql: &str) -> Vec<String> {
    let comment_free: String = sql
        .lines()
        .map(|l| l.split_once("--").map(|(code, _)| code).unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n");
    comment_free
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
