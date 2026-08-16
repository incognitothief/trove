//! SQLite-backed local databases.
//!
//! All of these are disposable caches/bookkeeping per ADR 000 — none is
//! authoritative. Opening a database applies its schema idempotently so a wiped
//! `~/.trove` rebuilds itself on demand.

pub mod archive;
pub mod fingerprint;
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

/// Idempotently add `column` to `table` if it doesn't already exist — the
/// shared building block every per-database `migrate_*_schema` function uses
/// so an older on-disk database picks up new columns without a real
/// migration system. Previously duplicated verbatim in `import.rs` and
/// `transfer.rs`; consolidated here rather than adding a third copy.
pub(crate) fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for col in cols {
        if col? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"),
        [],
    )?;
    Ok(())
}
