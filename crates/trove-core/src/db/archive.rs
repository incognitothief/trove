//! The local archive index cache (`~/.trove/archive.sqlite`).
//!
//! A disposable mirror of the canonical bucket index. It stores the reconcile
//! generation stamp in `meta` so a read can cheaply decide if it is current.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection, Row};

use super::{open_in_memory, open_with_schema, schema};
use crate::error::Result;
use crate::model::{ArchiveEntry, Metadata, TrackId};
use crate::query::QuerySpec;

/// Handle to the local archive index database.
pub struct ArchiveDb {
    conn: Connection,
}

impl ArchiveDb {
    /// Open (creating if needed) the archive database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(ArchiveDb {
            conn: open_with_schema(path, schema::ARCHIVE_SCHEMA)?,
        })
    }

    /// Open an ephemeral in-memory archive database (tests).
    pub fn in_memory() -> Result<Self> {
        Ok(ArchiveDb {
            conn: open_in_memory(schema::ARCHIVE_SCHEMA)?,
        })
    }

    /// The reconcile generation this cache was last hydrated to, if any.
    pub fn generation(&self) -> Result<Option<u64>> {
        let value: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'generation'", [], |r| {
                r.get(0)
            })
            .ok();
        Ok(value.and_then(|v| v.parse().ok()))
    }

    /// Record the generation this cache is now current to.
    pub fn set_generation(&self, generation: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('generation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![generation.to_string()],
        )?;
        Ok(())
    }

    /// Number of tracks currently cached.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Insert or update a single archive entry.
    pub fn upsert(&self, entry: &ArchiveEntry) -> Result<()> {
        let tags = serde_json::to_string(&entry.tags)?;
        self.conn.execute(
            "INSERT INTO tracks (
                track_id, object_key, size_bytes, sha256,
                title, artist, album, genre, year, bpm, key_camelot,
                duration_secs, comment, file_type, tags,
                imported_at, updated_at, source_path_original, artwork_object_key
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19
             )
             ON CONFLICT(track_id) DO UPDATE SET
                object_key = excluded.object_key,
                size_bytes = excluded.size_bytes,
                sha256 = excluded.sha256,
                title = excluded.title,
                artist = excluded.artist,
                album = excluded.album,
                genre = excluded.genre,
                year = excluded.year,
                bpm = excluded.bpm,
                key_camelot = excluded.key_camelot,
                duration_secs = excluded.duration_secs,
                comment = excluded.comment,
                file_type = excluded.file_type,
                tags = excluded.tags,
                updated_at = excluded.updated_at,
                source_path_original = excluded.source_path_original,
                artwork_object_key = excluded.artwork_object_key",
            params![
                entry.track_id.0,
                entry.object_key,
                entry.size_bytes as i64,
                entry.sha256,
                entry.metadata.title,
                entry.metadata.artist,
                entry.metadata.album,
                entry.metadata.genre,
                entry.metadata.year,
                entry.metadata.bpm,
                entry.metadata.key,
                entry.metadata.duration_secs,
                entry.metadata.comment,
                entry.metadata.file_type,
                tags,
                entry.imported_at.to_rfc3339(),
                entry.updated_at.to_rfc3339(),
                entry.source_path_original,
                entry.artwork_object_key,
            ],
        )?;
        Ok(())
    }

    /// Replace the entire cache contents from an iterator of entries.
    ///
    /// Used when fully rehydrating from the bucket index.
    pub fn replace_all<'a, I>(&mut self, entries: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a ArchiveEntry>,
    {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM tracks", [])?;
        {
            for entry in entries {
                let tags = serde_json::to_string(&entry.tags)?;
                tx.execute(
                    "INSERT INTO tracks (
                        track_id, object_key, size_bytes, sha256,
                        title, artist, album, genre, year, bpm, key_camelot,
                        duration_secs, comment, file_type, tags,
                        imported_at, updated_at, source_path_original, artwork_object_key
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                        ?14, ?15, ?16, ?17, ?18, ?19
                     )",
                    params![
                        entry.track_id.0,
                        entry.object_key,
                        entry.size_bytes as i64,
                        entry.sha256,
                        entry.metadata.title,
                        entry.metadata.artist,
                        entry.metadata.album,
                        entry.metadata.genre,
                        entry.metadata.year,
                        entry.metadata.bpm,
                        entry.metadata.key,
                        entry.metadata.duration_secs,
                        entry.metadata.comment,
                        entry.metadata.file_type,
                        tags,
                        entry.imported_at.to_rfc3339(),
                        entry.updated_at.to_rfc3339(),
                        entry.source_path_original,
                        entry.artwork_object_key,
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Return every cached entry (used when pushing the local index to the bucket).
    pub fn all(&self) -> Result<Vec<ArchiveEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM tracks ORDER BY track_id"))?;
        let rows = stmt.query_map([], row_to_entry)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// Fetch a single entry by content hash (archive-wide dedupe, ADR 004).
    pub fn find_by_sha256(&self, sha256: &str) -> Result<Option<ArchiveEntry>> {
        let entry = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM tracks WHERE sha256 = ?1 LIMIT 1"),
                params![sha256],
                row_to_entry,
            )
            .ok();
        entry.transpose()
    }

    /// Fetch a single entry by id.
    pub fn get(&self, id: &TrackId) -> Result<Option<ArchiveEntry>> {
        let entry = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM tracks WHERE track_id = ?1"),
                params![id.0],
                row_to_entry,
            )
            .ok();
        entry.transpose()
    }

    /// Execute a declarative query and return matching entries.
    pub fn query(&self, spec: &QuerySpec) -> Result<Vec<ArchiveEntry>> {
        let mut sql = format!("SELECT {COLUMNS} FROM tracks WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(text) = &spec.text {
            let pattern = like(text);
            let base = args.len();
            for _ in 0..4 {
                args.push(Value::Text(pattern.clone()));
            }
            sql.push_str(&format!(
                " AND (title LIKE ?{} OR artist LIKE ?{} OR album LIKE ?{} OR comment LIKE ?{})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
            ));
        }
        push_eq(&mut sql, &mut args, "artist", spec.artist.as_deref());
        push_eq(&mut sql, &mut args, "album", spec.album.as_deref());
        push_eq(&mut sql, &mut args, "genre", spec.genre.as_deref());
        push_eq(&mut sql, &mut args, "key_camelot", spec.key.as_deref());
        push_eq(&mut sql, &mut args, "file_type", spec.file_type.as_deref());

        if let Some(min) = spec.bpm.min {
            args.push(Value::Real(min as f64));
            sql.push_str(&format!(" AND bpm >= ?{}", args.len()));
        }
        if let Some(max) = spec.bpm.max {
            args.push(Value::Real(max as f64));
            sql.push_str(&format!(" AND bpm <= ?{}", args.len()));
        }
        if let Some(min) = spec.year.min {
            args.push(Value::Integer(min as i64));
            sql.push_str(&format!(" AND year >= ?{}", args.len()));
        }
        if let Some(max) = spec.year.max {
            args.push(Value::Integer(max as i64));
            sql.push_str(&format!(" AND year <= ?{}", args.len()));
        }

        sql.push_str(" ORDER BY artist, album, title");
        if let Some(limit) = spec.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), row_to_entry)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }

        // Tag filtering is applied in-memory (tags are a JSON column).
        if spec.tags.is_empty() {
            Ok(out)
        } else {
            Ok(out
                .into_iter()
                .filter(|e| spec.tags.iter().all(|t| e.tags.contains(t)))
                .collect())
        }
    }
}

const COLUMNS: &str = "track_id, object_key, size_bytes, sha256, title, artist, \
     album, genre, year, bpm, key_camelot, duration_secs, comment, file_type, \
     tags, imported_at, updated_at, source_path_original, artwork_object_key";

fn like(s: &str) -> String {
    format!("%{s}%")
}

fn push_eq(sql: &mut String, args: &mut Vec<Value>, column: &str, value: Option<&str>) {
    if let Some(v) = value {
        args.push(Value::Text(v.to_string()));
        sql.push_str(&format!(" AND {column} = ?{}", args.len()));
    }
}

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<Result<ArchiveEntry>> {
    // Inner Result carries our own error type for the JSON/date parsing.
    Ok((|| {
        let tags_json: String = row.get("tags")?;
        let tags: Vec<String> = serde_json::from_str(&tags_json)?;
        let imported_at = parse_dt(row.get::<_, String>("imported_at")?);
        let updated_at = parse_dt(row.get::<_, String>("updated_at")?);
        Ok(ArchiveEntry {
            track_id: TrackId(row.get("track_id")?),
            object_key: row.get("object_key")?,
            size_bytes: row.get::<_, i64>("size_bytes")? as u64,
            sha256: row.get("sha256")?,
            metadata: Metadata {
                title: row.get("title")?,
                artist: row.get("artist")?,
                album: row.get("album")?,
                genre: row.get("genre")?,
                year: row.get("year")?,
                bpm: row.get("bpm")?,
                key: row.get("key_camelot")?,
                duration_secs: row.get("duration_secs")?,
                comment: row.get("comment")?,
                file_type: row.get("file_type")?,
            },
            tags,
            imported_at,
            updated_at,
            source_path_original: row.get("source_path_original")?,
            artwork_object_key: row.get("artwork_object_key")?,
        })
    })())
}

fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
