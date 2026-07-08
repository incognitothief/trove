//! Resumable transfer/sync bookkeeping (`~/.trove/sync.sqlite`).
//!
//! Sync state is first-class so an interrupted flash can resume and be verified.
//! The bootstrap defines the transfer model and planning surface; the actual
//! byte-moving download loop (with retry/backoff and multipart resume) plugs in
//! behind [`plan_playlist_sync`].

use serde::{Deserialize, Serialize};

use crate::config::ExportLayout;
use crate::model::ArchiveEntry;

/// Direction of a transfer relative to the local host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Download,
    Upload,
}

/// One planned or in-flight transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub track_id: String,
    pub object_key: String,
    /// Destination path relative to the volume root.
    pub relative_path: String,
    pub direction: Direction,
    pub bytes_total: u64,
}

/// A resolved plan describing exactly what a sync will move.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncPlan {
    pub transfers: Vec<Transfer>,
    /// Tracks already present on the volume (skipped).
    pub already_present: Vec<String>,
    pub bytes_remaining: u64,
}

/// Build a sync plan for a set of tracks, computing destination paths from the
/// export layout. Presence checking against the volume DB is layered on top.
pub fn plan_playlist_sync(entries: &[ArchiveEntry], layout: ExportLayout) -> SyncPlan {
    let mut plan = SyncPlan::default();
    for entry in entries {
        let relative_path = destination_path(entry, layout);
        plan.bytes_remaining += entry.size_bytes;
        plan.transfers.push(Transfer {
            track_id: entry.track_id.0.clone(),
            object_key: entry.object_key.clone(),
            relative_path,
            direction: Direction::Download,
            bytes_total: entry.size_bytes,
        });
    }
    plan
}

/// Compute a track's destination path under `Music/` for a given layout.
pub fn destination_path(entry: &ArchiveEntry, layout: ExportLayout) -> String {
    let file_name = entry
        .object_key
        .rsplit('/')
        .next()
        .unwrap_or(&entry.object_key);
    match layout {
        ExportLayout::Flat => format!("Music/{file_name}"),
        ExportLayout::ArtistAlbum => {
            let artist = entry.metadata.artist.as_deref().unwrap_or("Unknown Artist");
            let album = entry.metadata.album.as_deref().unwrap_or("Unknown Album");
            format!(
                "Music/{}/{}/{}",
                sanitize(artist),
                sanitize(album),
                file_name
            )
        }
    }
}

fn sanitize(component: &str) -> String {
    component
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}
