//! Durable import job bookkeeping (`~/.trove/sync.sqlite`).
//!
//! In-flight bulk imports persist here so a crash mid-job can resume without
//! rescanning or rehashing finished files (ADR 005).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use super::{open_in_memory, open_with_schema, schema};
use crate::error::{Error, Result};
use crate::import::{
    ArtworkCandidate, FileState, ImportJob, ImportOptions, ImportStats, Phase, PlannedFile,
};
use crate::model::TrackId;

/// Summary row for `import list` and HTTP list endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct ImportJobSummary {
    pub id: String,
    pub source_root: PathBuf,
    pub phase: Phase,
    pub total_files: usize,
    pub stats: ImportStats,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Structured per-job status (phase + per-state counts).
#[derive(Debug, Clone, Serialize)]
pub struct ImportStatusReport {
    pub id: String,
    pub source_root: PathBuf,
    pub phase: Phase,
    pub stats: ImportStats,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Handle to the sync/import SQLite database.
pub struct ImportDb {
    conn: Connection,
}

impl ImportDb {
    /// Open (creating if needed) the sync database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_with_schema(path, schema::SYNC_SCHEMA)?;
        migrate_import_schema(&conn)?;
        Ok(ImportDb { conn })
    }

    /// Open an ephemeral in-memory sync database (tests).
    pub fn in_memory() -> Result<Self> {
        let conn = open_in_memory(schema::SYNC_SCHEMA)?;
        migrate_import_schema(&conn)?;
        Ok(ImportDb { conn })
    }

    /// Persist a full job snapshot (job row + all file rows).
    pub fn save_job(&self, job: &ImportJob, options: &ImportOptions) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let created_at: String = self
            .conn
            .query_row(
                "SELECT created_at FROM import_jobs WHERE id = ?1",
                params![job.id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| now.clone());
        let artwork_json = if job.artwork.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&job.artwork)?)
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO import_jobs (
                id, source_root, phase, staging_prefix, total_files,
                include_dotfiles, capture_artwork, artwork_json,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                source_root = excluded.source_root,
                phase = excluded.phase,
                staging_prefix = excluded.staging_prefix,
                total_files = excluded.total_files,
                include_dotfiles = excluded.include_dotfiles,
                capture_artwork = excluded.capture_artwork,
                artwork_json = excluded.artwork_json,
                updated_at = excluded.updated_at",
            params![
                job.id,
                job.source_root.display().to_string(),
                job.phase.as_str(),
                job.staging_prefix,
                job.files.len() as i64,
                options.include_dotfiles as i32,
                options.capture_artwork as i32,
                artwork_json,
                created_at,
                now,
            ],
        )?;
        tx.execute(
            "DELETE FROM import_files WHERE job_id = ?1",
            params![job.id],
        )?;
        for file in &job.files {
            insert_file(&tx, &job.id, file)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load a job by id, including all file rows and stored options.
    pub fn load_job(&self, id: &str) -> Result<(ImportJob, ImportOptions)> {
        let row = self
            .conn
            .query_row(
                "SELECT source_root, phase, staging_prefix, include_dotfiles, capture_artwork, artwork_json
                 FROM import_jobs WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i32>(3)? != 0,
                        r.get::<_, i32>(4)? != 0,
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .map_err(|_| Error::not_found(format!("import job '{id}'")))?;

        let (
            source_root,
            phase_str,
            staging_prefix,
            include_dotfiles,
            capture_artwork,
            artwork_json,
        ) = row;
        let phase = Phase::parse(&phase_str)
            .ok_or_else(|| Error::Other(format!("invalid phase '{phase_str}' for job {id}")))?;
        let artwork: Vec<ArtworkCandidate> = match artwork_json {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        };
        let options = ImportOptions {
            include_dotfiles,
            capture_artwork,
        };

        let mut stmt = self.conn.prepare(
            "SELECT path, size, mtime, sha256, state, s3_object_key, track_id, etag, error, attempts
             FROM import_files WHERE job_id = ?1 ORDER BY path",
        )?;
        let files = stmt
            .query_map(params![id], row_to_planned_file)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let job = ImportJob {
            id: id.to_string(),
            source_root: PathBuf::from(source_root),
            staging_prefix,
            phase,
            files,
            artwork,
        };
        Ok((job, options))
    }

    /// Update a single file row after a state transition.
    pub fn update_file(&self, job_id: &str, file: &PlannedFile) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        insert_file(&tx, job_id, file)?;
        tx.execute(
            "UPDATE import_jobs SET updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), job_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Update only the job phase (and timestamp).
    pub fn update_phase(&self, job_id: &str, phase: Phase) -> Result<()> {
        self.conn.execute(
            "UPDATE import_jobs SET phase = ?1, updated_at = ?2 WHERE id = ?3",
            params![phase.as_str(), Utc::now().to_rfc3339(), job_id],
        )?;
        Ok(())
    }

    /// List import jobs. Default: incomplete jobs; `all` includes finished jobs.
    pub fn list_jobs(&self, all: bool) -> Result<Vec<ImportJobSummary>> {
        let sql = if all {
            "SELECT id, source_root, phase, total_files, created_at, updated_at
             FROM import_jobs ORDER BY updated_at DESC"
        } else {
            "SELECT id, source_root, phase, total_files, created_at, updated_at
             FROM import_jobs
             WHERE phase != 'done'
                OR EXISTS (
                    SELECT 1 FROM import_files f
                    WHERE f.job_id = import_jobs.id AND f.state = 'failed'
                )
             ORDER BY updated_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as usize,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, source_root, phase_str, total_files, created_at, updated_at) = row?;
            let phase = Phase::parse(&phase_str)
                .ok_or_else(|| Error::Other(format!("invalid phase '{phase_str}'")))?;
            let stats = self.file_stats(&id)?;
            out.push(ImportJobSummary {
                id,
                source_root: PathBuf::from(source_root),
                phase,
                total_files,
                stats,
                created_at: parse_ts(&created_at)?,
                updated_at: parse_ts(&updated_at)?,
            });
        }
        Ok(out)
    }

    /// Structured status for a single job.
    pub fn status(&self, job_id: &str) -> Result<ImportStatusReport> {
        let row = self
            .conn
            .query_row(
                "SELECT source_root, phase, created_at, updated_at
                 FROM import_jobs WHERE id = ?1",
                params![job_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|_| Error::not_found(format!("import job '{job_id}'")))?;
        let (source_root, phase_str, created_at, updated_at) = row;
        let phase = Phase::parse(&phase_str)
            .ok_or_else(|| Error::Other(format!("invalid phase '{phase_str}'")))?;
        Ok(ImportStatusReport {
            id: job_id.to_string(),
            source_root: PathBuf::from(source_root),
            phase,
            stats: self.file_stats(job_id)?,
            created_at: parse_ts(&created_at)?,
            updated_at: parse_ts(&updated_at)?,
        })
    }

    fn file_stats(&self, job_id: &str) -> Result<ImportStats> {
        let mut stats = ImportStats::default();
        let mut stmt = self
            .conn
            .prepare("SELECT state FROM import_files WHERE job_id = ?1")?;
        let states = stmt.query_map(params![job_id], |r| r.get::<_, String>(0))?;
        for state in states {
            let state = state?;
            stats.total += 1;
            match state.as_str() {
                "duplicate" => stats.duplicates += 1,
                "uploaded" => stats.uploaded += 1,
                "verified" => stats.verified += 1,
                "committed" => stats.committed += 1,
                "failed" => stats.failed += 1,
                _ => {}
            }
        }
        Ok(stats)
    }
}

fn insert_file(conn: &Connection, job_id: &str, file: &PlannedFile) -> Result<()> {
    conn.execute(
        "INSERT INTO import_files (
            job_id, path, size, mtime, sha256, metadata_extracted, state,
            s3_object_key, track_id, etag, error, attempts
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(job_id, path) DO UPDATE SET
            size = excluded.size,
            mtime = excluded.mtime,
            sha256 = excluded.sha256,
            state = excluded.state,
            s3_object_key = excluded.s3_object_key,
            track_id = excluded.track_id,
            etag = excluded.etag,
            error = excluded.error,
            attempts = excluded.attempts",
        params![
            job_id,
            file.path.display().to_string(),
            file.size as i64,
            file.mtime,
            file.sha256,
            file.state.as_str(),
            file.object_key,
            file.track_id.as_ref().map(|t| t.0.clone()),
            file.etag,
            file.error,
            file.attempts as i64,
        ],
    )?;
    Ok(())
}

fn row_to_planned_file(r: &Row<'_>) -> rusqlite::Result<PlannedFile> {
    let path: String = r.get(0)?;
    let size: i64 = r.get(1)?;
    let mtime: Option<String> = r.get(2)?;
    let sha256: String = r.get(3)?;
    let state_str: String = r.get(4)?;
    let object_key: Option<String> = r.get(5)?;
    let track_id: Option<String> = r.get(6)?;
    let etag: Option<String> = r.get(7)?;
    let error: Option<String> = r.get(8)?;
    let attempts: i64 = r.get(9)?;
    let state = FileState::parse(&state_str).unwrap_or(FileState::Failed);
    Ok(PlannedFile {
        path: PathBuf::from(path),
        size: size as u64,
        mtime,
        sha256,
        state,
        object_key,
        track_id: track_id.map(TrackId),
        etag,
        error,
        attempts: attempts as u32,
    })
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::Other(format!("invalid timestamp '{s}': {e}")))
}

/// Apply additive migrations for databases created before ADR 005 columns.
fn migrate_import_schema(conn: &Connection) -> Result<()> {
    ensure_column(
        conn,
        "import_jobs",
        "include_dotfiles",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "import_jobs",
        "capture_artwork",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(conn, "import_jobs", "artwork_json", "TEXT")?;
    ensure_column(conn, "import_files", "track_id", "TEXT")?;
    ensure_column(
        conn,
        "import_files",
        "attempts",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
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

#[derive(Serialize, Deserialize)]
struct ArtworkCandidateRow {
    path: PathBuf,
    sha256: String,
    size: u64,
    source_folder: String,
    object_key: Option<String>,
}

impl Serialize for ArtworkCandidate {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        ArtworkCandidateRow {
            path: self.path.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
            source_folder: self.source_folder.clone(),
            object_key: self.object_key.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ArtworkCandidate {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let row = ArtworkCandidateRow::deserialize(deserializer)?;
        Ok(ArtworkCandidate {
            path: row.path,
            sha256: row.sha256,
            size: row.size,
            source_folder: row.source_folder,
            object_key: row.object_key,
        })
    }
}
