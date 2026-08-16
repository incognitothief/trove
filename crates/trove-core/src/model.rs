//! Domain types shared across the core.
//!
//! These mirror the entities described in ADR 000: archive index entries,
//! tracks, playlists, volumes, and import bookkeeping.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a track within the archive.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId(pub String);

impl TrackId {
    pub fn new() -> Self {
        TrackId(Uuid::new_v4().to_string())
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for TrackId {
    fn from(s: &str) -> Self {
        TrackId(s.to_string())
    }
}

/// Audio/track metadata as extracted from files and stored in the index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    /// Beats per minute.
    pub bpm: Option<f32>,
    /// Musical key, Camelot or standard notation (e.g. "8A", "Am").
    pub key: Option<String>,
    /// Duration in seconds.
    pub duration_secs: Option<f64>,
    pub comment: Option<String>,
    pub file_type: Option<String>,
}

/// One entry of the canonical bucket index (see ADR "Bucket-side index").
///
/// This is the interchange shape serialized into `archive-index.jsonl` and
/// mirrored into `archive-index.sqlite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub track_id: TrackId,
    pub object_key: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default)]
    pub tags: Vec<String>,
    pub imported_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_path_original: Option<String>,
    pub artwork_object_key: Option<String>,
    /// Portable identity: this file's path relative to the declared library
    /// root, in canonical form (ADR 007, Group D2) — `/`-separated, no
    /// leading slash, NFC-normalized. Distinct from `source_path_original`,
    /// which stays the absolute, single-machine, one-shot snapshot; this is
    /// what recognizes "the same library, remounted somewhere else" across
    /// drives. `None` when no library root was declared at commit time (or
    /// for entries committed before this field existed — additive, not
    /// backfilled automatically; see `library::backfill_slugs`, Group D2a).
    #[serde(default)]
    pub library_relative_path: Option<String>,
}

/// A logical Trove playlist/crate (not yet a Mixxx playlist).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub track_ids: Vec<TrackId>,
}

impl Playlist {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Playlist {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            created_at: now,
            updated_at: now,
            track_ids: Vec::new(),
        }
    }
}

/// Tiny identity file written to a performance volume (`.trove-volume.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeIdentity {
    pub volume_id: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl VolumeIdentity {
    pub fn new(label: Option<String>) -> Self {
        VolumeIdentity {
            volume_id: Uuid::new_v4().to_string(),
            label,
            created_at: Utc::now(),
        }
    }
}

/// A captured cover-art object and its provenance (ADR 002).
///
/// Art is captured at import time (folder images live only on the disposable
/// source), content-addressed for dedup, and logged here so a later curation
/// analyzer can associate it with albums without needing the source device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkRecord {
    pub sha256: String,
    /// Content-addressed object key in the bucket (e.g. `artwork/<sha256>.jpg`).
    pub object_key: String,
    pub size_bytes: u64,
    /// Source folder the image was found in (provenance for association).
    pub source_folder: String,
    pub file_name: String,
    pub captured_at: DateTime<Utc>,
}

/// Reconciliation generation marker (`schema-version.json`), used to decide
/// whether the local cache is current before serving a read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub schema_version: u32,
    /// Monotonic generation stamp advanced on every canonical index write.
    pub generation: u64,
    pub updated_at: DateTime<Utc>,
}
