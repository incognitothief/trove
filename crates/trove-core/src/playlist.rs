//! Logical Trove playlists/crates (`~/.trove/playlists.sqlite`).
//!
//! Playlists are logical selections first, exported to portable `.m3u8` files
//! second. This module owns the logical side; export lives with volume sync.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection};

use crate::db::{open_in_memory, open_with_schema, schema};
use crate::error::{Error, Result};
use crate::model::{Playlist, TrackId};

/// Handle to the logical playlists database.
pub struct PlaylistDb {
    conn: Connection,
}

impl PlaylistDb {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(PlaylistDb {
            conn: open_with_schema(path, schema::PLAYLISTS_SCHEMA)?,
        })
    }

    pub fn in_memory() -> Result<Self> {
        Ok(PlaylistDb {
            conn: open_in_memory(schema::PLAYLISTS_SCHEMA)?,
        })
    }

    /// Create a new, empty playlist. Errors if the name already exists.
    pub fn create(&self, name: &str) -> Result<Playlist> {
        let playlist = Playlist::new(name);
        self.conn.execute(
            "INSERT INTO playlists(id, name, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![
                playlist.id,
                playlist.name,
                playlist.created_at.to_rfc3339(),
                playlist.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(playlist)
    }

    /// Look up a playlist id by its (unique) name.
    pub fn id_by_name(&self, name: &str) -> Result<String> {
        self.conn
            .query_row(
                "SELECT id FROM playlists WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .map_err(|_| Error::not_found(format!("playlist '{name}'")))
    }

    /// Add tracks to a playlist (appended after the current last position).
    pub fn add_tracks(&self, name: &str, track_ids: &[TrackId]) -> Result<()> {
        let id = self.id_by_name(name)?;
        let base: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        for (offset, track_id) in track_ids.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO playlist_tracks(playlist_id, track_id, position)
                 VALUES(?1, ?2, ?3)
                 ON CONFLICT(playlist_id, track_id) DO NOTHING",
                params![id, track_id.0, base + offset as i64],
            )?;
        }
        self.touch(&id)?;
        Ok(())
    }

    /// Remove a track from a playlist.
    pub fn remove_track(&self, name: &str, track_id: &TrackId) -> Result<()> {
        let id = self.id_by_name(name)?;
        self.conn.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
            params![id, track_id.0],
        )?;
        self.touch(&id)?;
        Ok(())
    }

    /// List all playlists (without their tracks).
    pub fn list(&self) -> Result<Vec<Playlist>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at, updated_at FROM playlists ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Playlist {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: parse_dt(r.get::<_, String>(2)?),
                updated_at: parse_dt(r.get::<_, String>(3)?),
                track_ids: Vec::new(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fetch a playlist and its ordered track ids.
    pub fn get(&self, name: &str) -> Result<Playlist> {
        let mut playlist = self.conn.query_row(
            "SELECT id, name, created_at, updated_at FROM playlists WHERE name = ?1",
            params![name],
            |r| {
                Ok(Playlist {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: parse_dt(r.get::<_, String>(2)?),
                    updated_at: parse_dt(r.get::<_, String>(3)?),
                    track_ids: Vec::new(),
                })
            },
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![playlist.id], |r| r.get::<_, String>(0))?;
        for row in rows {
            playlist.track_ids.push(TrackId(row?));
        }
        Ok(playlist)
    }

    fn touch(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

fn parse_dt(s: String) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
