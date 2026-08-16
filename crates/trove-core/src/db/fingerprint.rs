//! Persistent, path-keyed fingerprint cache (ADR 007, Group C1).
//!
//! Purely a performance cache — never a source of truth. If it's stale,
//! missing, or corrupt, callers fall back to reading and hashing the file;
//! nothing about correctness depends on this cache being right, only on it
//! either matching or being safely ignored.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection};

use super::{open_in_memory, open_with_schema, schema};
use crate::error::Result;

/// Handle to the local fingerprint cache database.
pub struct FingerprintCache {
    conn: Connection,
}

impl FingerprintCache {
    /// Open (creating if needed) the fingerprint cache at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(FingerprintCache {
            conn: open_with_schema(path, schema::FINGERPRINT_SCHEMA)?,
        })
    }

    /// Open an ephemeral in-memory fingerprint cache (tests).
    pub fn in_memory() -> Result<Self> {
        Ok(FingerprintCache {
            conn: open_in_memory(schema::FINGERPRINT_SCHEMA)?,
        })
    }

    /// Look up a cached hash for `path`, valid only if the cached size and
    /// mtime still match what's passed in — a mismatch (or no row) means the
    /// file has changed, or was never cached, and must be read and hashed.
    pub fn lookup(&self, path: &Path, size: u64, mtime: Option<&str>) -> Result<Option<String>> {
        let path_str = path.to_string_lossy();
        let row: Option<(i64, Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT size_bytes, mtime, sha256 FROM fingerprints WHERE path = ?1",
                params![path_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        Ok(row.and_then(|(cached_size, cached_mtime, sha256)| {
            if cached_size as u64 == size && cached_mtime.as_deref() == mtime {
                Some(sha256)
            } else {
                None
            }
        }))
    }

    /// Record (or update) the fingerprint for `path`.
    pub fn upsert(&self, path: &Path, size: u64, mtime: Option<&str>, sha256: &str) -> Result<()> {
        let path_str = path.to_string_lossy();
        self.conn.execute(
            "INSERT INTO fingerprints (path, size_bytes, mtime, sha256, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                mtime = excluded.mtime,
                sha256 = excluded.sha256,
                updated_at = excluded.updated_at",
            params![path_str, size as i64, mtime, sha256, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn lookup_misses_until_upserted_then_hits_on_matching_stat() {
        let cache = FingerprintCache::in_memory().unwrap();
        let path = PathBuf::from("/music/track.mp3");

        assert_eq!(cache.lookup(&path, 1024, Some("100.0")).unwrap(), None);

        cache.upsert(&path, 1024, Some("100.0"), "abc123").unwrap();
        assert_eq!(
            cache.lookup(&path, 1024, Some("100.0")).unwrap(),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn lookup_misses_when_size_or_mtime_changed() {
        let cache = FingerprintCache::in_memory().unwrap();
        let path = PathBuf::from("/music/track.mp3");
        cache.upsert(&path, 1024, Some("100.0"), "abc123").unwrap();

        assert_eq!(cache.lookup(&path, 2048, Some("100.0")).unwrap(), None);
        assert_eq!(cache.lookup(&path, 1024, Some("200.0")).unwrap(), None);
        assert_eq!(cache.lookup(&path, 1024, None).unwrap(), None);
    }

    #[test]
    fn upsert_overwrites_stale_entry_for_same_path() {
        let cache = FingerprintCache::in_memory().unwrap();
        let path = PathBuf::from("/music/track.mp3");
        cache.upsert(&path, 1024, Some("100.0"), "old-hash").unwrap();
        cache.upsert(&path, 2048, Some("200.0"), "new-hash").unwrap();

        assert_eq!(cache.lookup(&path, 1024, Some("100.0")).unwrap(), None);
        assert_eq!(
            cache.lookup(&path, 2048, Some("200.0")).unwrap(),
            Some("new-hash".to_string())
        );
    }
}
