//! Forward-only schema migrations (doc 03 §3, doc 16 M0 gate).
//!
//! The M0 validation gate requires that the schema round-trips **every**
//! [`aperture_contracts::EventType`]; see `crates/gates/tests`.

/// Migrations embedded at compile time, applied in order. `schema_migrations`
/// records the highest applied `version`.
pub const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

/// Apply every migration whose version is newer than `schema_migrations.MAX(version)`.
// pub fn run(conn: &rusqlite::Connection) -> Result<(), crate::DbError> { ... }
pub fn pending_after(applied_version: i64) -> impl Iterator<Item = &'static (i64, &'static str)> {
    MIGRATIONS.iter().filter(move |(v, _)| *v > applied_version)
}
