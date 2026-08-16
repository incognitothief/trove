//! Durable sync/transfer bookkeeping in `~/.trove/sync.sqlite` (ADR 006).

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

use super::{ensure_column, open_in_memory, open_with_schema, schema};
use crate::error::{Error, Result};
use crate::sync::{Direction, Transfer};

/// Lifecycle of a volume sync job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncJobStatus {
    Active,
    Done,
}

impl SyncJobStatus {
    fn as_str(self) -> &'static str {
        match self {
            SyncJobStatus::Active => "active",
            SyncJobStatus::Done => "done",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(SyncJobStatus::Active),
            "done" => Some(SyncJobStatus::Done),
            _ => None,
        }
    }
}

/// Status of one transfer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    Pending,
    Active,
    Done,
    Failed,
}

impl TransferStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferStatus::Pending => "pending",
            TransferStatus::Active => "active",
            TransferStatus::Done => "done",
            TransferStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TransferStatus::Pending),
            "active" => Some(TransferStatus::Active),
            "done" => Some(TransferStatus::Done),
            "failed" => Some(TransferStatus::Failed),
            _ => None,
        }
    }
}

/// Summary of a sync job for list/status surfaces.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncJobSummary {
    pub id: String,
    pub volume_id: String,
    pub mount_point: String,
    pub playlist_name: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A persisted transfer row.
#[derive(Debug, Clone)]
pub struct TransferRow {
    pub id: String,
    pub job_id: String,
    pub volume_id: String,
    pub track_id: String,
    pub object_key: String,
    pub relative_path: String,
    pub local_path: Option<String>,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub status: TransferStatus,
    pub attempts: u32,
}

pub struct TransferDb {
    conn: Connection,
}

impl TransferDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_with_schema(path, schema::SYNC_SCHEMA)?;
        migrate_transfer_schema(&conn)?;
        Ok(TransferDb { conn })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = open_in_memory(schema::SYNC_SCHEMA)?;
        migrate_transfer_schema(&conn)?;
        Ok(TransferDb { conn })
    }

    pub fn create_job(
        &self,
        volume_id: &str,
        mount_point: &str,
        playlist_name: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sync_jobs (
                id, volume_id, mount_point, playlist_name, status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                id,
                volume_id,
                mount_point,
                playlist_name,
                SyncJobStatus::Active.as_str(),
                now,
            ],
        )?;
        Ok(id)
    }

    pub fn finish_job(&self, job_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                SyncJobStatus::Done.as_str(),
                Utc::now().to_rfc3339(),
                job_id
            ],
        )?;
        Ok(())
    }

    pub fn insert_transfer(
        &self,
        job_id: &str,
        volume_id: &str,
        transfer: &Transfer,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO transfers (
                id, job_id, volume_id, track_id, direction, object_key, relative_path,
                local_path, bytes_total, bytes_done, status, attempts, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, 0, ?9, 0, ?10)",
            params![
                id,
                job_id,
                volume_id,
                transfer.track_id,
                direction_str(transfer.direction),
                transfer.object_key,
                transfer.relative_path,
                transfer.bytes_total as i64,
                TransferStatus::Pending.as_str(),
                now,
            ],
        )?;
        Ok(id)
    }

    pub fn list_incomplete(&self, job_id: &str) -> Result<Vec<TransferRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, volume_id, track_id, object_key, relative_path, local_path,
                    bytes_total, bytes_done, status, attempts
             FROM transfers
             WHERE job_id = ?1 AND status IN ('pending', 'active', 'failed')
             ORDER BY relative_path",
        )?;
        let rows = stmt.query_map(params![job_id], row_to_transfer)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn latest_active_job(&self) -> Result<Option<SyncJobSummary>> {
        match self.conn.query_row(
            "SELECT id, volume_id, mount_point, playlist_name, status, created_at, updated_at
             FROM sync_jobs
             WHERE status = 'active'
             ORDER BY updated_at DESC LIMIT 1",
            [],
            row_to_sync_job,
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_job(&self, job_id: &str) -> Result<SyncJobSummary> {
        self.conn
            .query_row(
                "SELECT id, volume_id, mount_point, playlist_name, status, created_at, updated_at
                 FROM sync_jobs WHERE id = ?1",
                params![job_id],
                row_to_sync_job,
            )
            .map_err(|_| Error::not_found(format!("sync job '{job_id}'")))
    }

    pub fn update_transfer(
        &self,
        id: &str,
        status: TransferStatus,
        local_path: Option<&str>,
        bytes_done: u64,
        attempts: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET status = ?1, local_path = ?2, bytes_done = ?3,
             attempts = ?4, updated_at = ?5 WHERE id = ?6",
            params![
                status.as_str(),
                local_path,
                bytes_done as i64,
                attempts,
                Utc::now().to_rfc3339(),
                id,
            ],
        )?;
        Ok(())
    }
}

fn direction_str(direction: Direction) -> &'static str {
    match direction {
        Direction::Download => "download",
        Direction::Upload => "upload",
    }
}

fn row_to_transfer(row: &Row<'_>) -> rusqlite::Result<TransferRow> {
    let status_str: String = row.get(9)?;
    let status = TransferStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(9, status_str, rusqlite::types::Type::Text)
    })?;
    Ok(TransferRow {
        id: row.get(0)?,
        job_id: row.get(1)?,
        volume_id: row.get(2)?,
        track_id: row.get(3)?,
        object_key: row.get(4)?,
        relative_path: row.get(5)?,
        local_path: row.get(6)?,
        bytes_total: row.get::<_, i64>(7)? as u64,
        bytes_done: row.get::<_, i64>(8)? as u64,
        status,
        attempts: row.get(10)?,
    })
}

fn row_to_sync_job(row: &Row<'_>) -> rusqlite::Result<SyncJobSummary> {
    let status_str: String = row.get(4)?;
    let _ = SyncJobStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(4, status_str.clone(), rusqlite::types::Type::Text)
    })?;
    let created_at: String = row.get(5)?;
    let updated_at: String = row.get(6)?;
    Ok(SyncJobSummary {
        id: row.get(0)?,
        volume_id: row.get(1)?,
        mount_point: row.get(2)?,
        playlist_name: row.get(3)?,
        status: status_str,
        created_at: parse_ts(&created_at).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )))
        })?,
        updated_at: parse_ts(&updated_at).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )))
        })?,
    })
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::Other(format!("invalid timestamp '{s}': {e}")))
}

fn migrate_transfer_schema(conn: &Connection) -> Result<()> {
    ensure_column(conn, "transfers", "job_id", "TEXT")?;
    ensure_column(conn, "transfers", "volume_id", "TEXT")?;
    ensure_column(conn, "transfers", "relative_path", "TEXT")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_jobs (
            id            TEXT PRIMARY KEY,
            volume_id     TEXT NOT NULL,
            mount_point   TEXT NOT NULL,
            playlist_name TEXT,
            status        TEXT NOT NULL,
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
        );",
    )?;
    Ok(())
}

