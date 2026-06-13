//! Storage layer (doc 03): SQLite + sqlite-vec, the single durable backbone.
//!
//! The bus (doc 15 §1) is at-most-once; **this is the truth**. Only the Tier-0
//! pipeline writes; the pattern engine, retrieval, payload builder, and UI read.
//!
//! At-rest encryption (SQLCipher-style page encryption, key wrapped by Windows
//! DPAPI — doc 13 §6) is applied when the connection is opened; the key is
//! supplied by `aperture-privacy::key_manager`. Loss of the key => DB
//! unreadable, **by design**.

pub mod migrations;
pub mod retention;

use std::path::PathBuf;

/// `%LOCALAPPDATA%\Aperture\history.db` (doc 03 §1).
pub fn default_db_path() -> PathBuf {
    // TODO(M0): resolve %LOCALAPPDATA% via `dirs`/`known-folders`; create the dir.
    PathBuf::from(r"%LOCALAPPDATA%\Aperture\history.db")
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("the database could not be decrypted (wrong or missing key)")]
    Decryption,
}

/// A handle to the encrypted history DB. Wraps a `rusqlite::Connection` plus the
/// loaded sqlite-vec extension.
pub struct Db {
    // conn: rusqlite::Connection,
}

impl Db {
    /// Open (creating if needed) the encrypted DB, load sqlite-vec, and run
    /// pending migrations. `wrapped_key` comes from `aperture-privacy` (doc 13 §6).
    pub fn open_encrypted(_path: PathBuf, _wrapped_key: &[u8]) -> Result<Self, DbError> {
        // TODO(M0/M9):
        //   1. open rusqlite connection; PRAGMA key = <unwrapped> (SQLCipher).
        //   2. load the sqlite-vec extension.
        //   3. PRAGMA journal_mode = WAL.
        //   4. migrations::run(&conn).
        todo!("M0: connect + migrate; M9: wire SQLCipher key + sqlite-vec")
    }

    /// One-click Purge All: truncate every table + VACUUM (doc 03 §6, doc 13 §7).
    /// Audit rows (`capture_toggle`, `cloud_send`) survive 30 d, then expire.
    pub fn purge_all(&self) -> Result<(), DbError> {
        todo!("M9: truncate + VACUUM, preserving audit rows for 30 d")
    }
}
