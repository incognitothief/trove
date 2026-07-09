//! Per-volume SQLite cache (`~/.trove/volumes/{volume_id}.sqlite`).

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Row};

use super::{open_in_memory, open_with_schema, schema};
use crate::error::Result;
use crate::model::TrackId;

/// Status of a file on a performance volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeFileStatus {
    Copied,
    Pending,
    Stale,
    Failed,
}

impl VolumeFileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            VolumeFileStatus::Copied => "copied",
            VolumeFileStatus::Pending => "pending",
            VolumeFileStatus::Stale => "stale",
            VolumeFileStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "copied" => Some(VolumeFileStatus::Copied),
            "pending" => Some(VolumeFileStatus::Pending),
            "stale" => Some(VolumeFileStatus::Stale),
            "failed" => Some(VolumeFileStatus::Failed),
            _ => None,
        }
    }
}

/// One row in the volume `files` table.
#[derive(Debug, Clone)]
pub struct VolumeFile {
    pub track_id: TrackId,
    pub relative_path: String,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub status: VolumeFileStatus,
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// Summary counts for `volume status`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VolumeFileStats {
    pub copied: usize,
    pub pending: usize,
    pub stale: usize,
    pub failed: usize,
    pub total: usize,
}

/// Host-side index of a removable performance volume.
pub struct VolumeDb {
    conn: Connection,
}

impl VolumeDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_with_schema(path, schema::VOLUME_SCHEMA)?;
        Ok(VolumeDb { conn })
    }

    pub fn in_memory() -> Result<Self> {
        Ok(VolumeDb {
            conn: open_in_memory(schema::VOLUME_SCHEMA)?,
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO volume (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM volume WHERE key = ?1",
            params![key],
            |r| r.get(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert_file(&self, file: &VolumeFile) -> Result<()> {
        let last_seen = file
            .last_seen_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        self.conn.execute(
            "INSERT INTO files (
                track_id, relative_path, size_bytes, mtime, sha256, status, last_seen_at
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)
             ON CONFLICT(track_id) DO UPDATE SET
                relative_path = excluded.relative_path,
                size_bytes = excluded.size_bytes,
                sha256 = excluded.sha256,
                status = excluded.status,
                last_seen_at = excluded.last_seen_at",
            params![
                file.track_id.0,
                file.relative_path,
                file.size_bytes.map(|s| s as i64),
                file.sha256,
                file.status.as_str(),
                last_seen,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_track_id(&self, track_id: &TrackId) -> Result<Option<VolumeFile>> {
        match self.conn.query_row(
            "SELECT track_id, relative_path, size_bytes, sha256, status, last_seen_at
                 FROM files WHERE track_id = ?1",
            params![track_id.0],
            |row| Ok(row_to_volume_file(row)),
        ) {
            Ok(row) => Ok(Some(row?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_by_sha256(&self, sha256: &str) -> Result<Option<VolumeFile>> {
        match self.conn.query_row(
            "SELECT track_id, relative_path, size_bytes, sha256, status, last_seen_at
                 FROM files WHERE sha256 = ?1 AND status = 'copied'",
            params![sha256],
            |row| Ok(row_to_volume_file(row)),
        ) {
            Ok(row) => Ok(Some(row?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn stats(&self) -> Result<VolumeFileStats> {
        let mut stmt = self
            .conn
            .prepare("SELECT status, COUNT(*) FROM files GROUP BY status")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut stats = VolumeFileStats::default();
        for row in rows {
            let (status, count) = row?;
            let n = count as usize;
            stats.total += n;
            match status.as_str() {
                "copied" => stats.copied = n,
                "pending" => stats.pending = n,
                "stale" => stats.stale = n,
                "failed" => stats.failed = n,
                _ => {}
            }
        }
        Ok(stats)
    }

    pub fn all_files(&self) -> Result<Vec<VolumeFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT track_id, relative_path, size_bytes, sha256, status, last_seen_at
             FROM files ORDER BY relative_path",
        )?;
        let rows = stmt.query_map([], row_to_volume_file)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn row_to_volume_file(row: &Row<'_>) -> rusqlite::Result<VolumeFile> {
    let status_str: String = row.get(4)?;
    let status = VolumeFileStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(4, status_str, rusqlite::types::Type::Text)
    })?;
    let last_seen: Option<String> = row.get(5)?;
    Ok(VolumeFile {
        track_id: TrackId(row.get(0)?),
        relative_path: row.get(1)?,
        size_bytes: row.get::<_, Option<i64>>(2)?.map(|s| s as u64),
        sha256: row.get(3)?,
        status,
        last_seen_at: last_seen.as_deref().and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
    })
}
