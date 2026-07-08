//! SQLite-backed local databases.
//!
//! All of these are disposable caches/bookkeeping per ADR 000 — none is
//! authoritative. Opening a database applies its schema idempotently so a wiped
//! `~/.trove` rebuilds itself on demand.

pub mod archive;
pub mod import;
pub mod schema;
pub mod transfer;
pub mod volume;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Open (creating if needed) a SQLite database at `path` and apply `schema`.
pub fn open_with_schema(path: &Path, schema: &str) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(schema)?;
    Ok(conn)
}

/// Open an in-memory database with `schema` applied (used in tests).
pub fn open_in_memory(schema: &str) -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(schema)?;
    Ok(conn)
}
