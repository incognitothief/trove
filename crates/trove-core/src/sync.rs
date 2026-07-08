//! Resumable transfer/sync for flashing performance volumes (ADR 006).
//!
//! Sync state persists in `~/.trove/sync.sqlite`; per-volume file index lives in
//! `~/.trove/volumes/{volume_id}.sqlite`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{ExportLayout, PlaylistFormat};
use crate::db::transfer::{TransferDb, TransferStatus};
use crate::db::volume::{VolumeDb, VolumeFile, VolumeFileStatus};
use crate::error::{Error, Result};
use crate::model::{ArchiveEntry, TrackId};
use crate::store::ObjectStore;

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

/// Portable playlist export payload.
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistExport {
    pub filename: String,
    pub body: String,
    pub relative_paths: Vec<String>,
}

/// Per-track diff result for a volume.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeDiffEntry {
    pub track_id: TrackId,
    pub relative_path: String,
    pub state: VolumeDiffState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeDiffState {
    Present,
    Missing,
    Stale,
}

const MAX_IO_ATTEMPTS: u32 = 5;

/// Build a sync plan, skipping tracks already on the volume (by sha256).
pub fn plan_sync(
    entries: &[ArchiveEntry],
    layout: ExportLayout,
    volume_db: Option<&VolumeDb>,
    mount: Option<&Path>,
) -> Result<SyncPlan> {
    let mut plan = SyncPlan::default();
    let mut used_paths: HashSet<String> = HashSet::new();

    for entry in entries {
        if let Some(db) = volume_db {
            if let Some(existing) = db.get_by_sha256(&entry.sha256)? {
                if existing.status == VolumeFileStatus::Copied {
                    if file_present_on_disk(mount, &existing.relative_path, entry.size_bytes) {
                        plan.already_present.push(entry.track_id.0.clone());
                        used_paths.insert(existing.relative_path);
                        continue;
                    }
                }
            }
        }

        let relative_path = unique_destination_path(entry, layout, &mut used_paths);
        plan.bytes_remaining += entry.size_bytes;
        plan.transfers.push(Transfer {
            track_id: entry.track_id.0.clone(),
            object_key: entry.object_key.clone(),
            relative_path,
            direction: Direction::Download,
            bytes_total: entry.size_bytes,
        });
    }
    Ok(plan)
}

/// Execute pending transfers for a sync job.
pub fn run_sync_job(
    mount: &Path,
    entries_by_id: &HashMap<String, ArchiveEntry>,
    transfer_db: &TransferDb,
    volume_db: &VolumeDb,
    store: &dyn ObjectStore,
    job_id: &str,
) -> Result<(usize, usize)> {
    let rows = transfer_db.list_incomplete(job_id)?;
    let mut done = 0usize;
    let mut failed = 0usize;

    for row in rows {
        let entry = entries_by_id.get(&row.track_id).ok_or_else(|| {
            Error::not_found(format!("archive entry for track '{}'", row.track_id))
        })?;

        let dest = mount.join(&row.relative_path);
        let mut attempts = row.attempts;
        transfer_db.update_transfer(&row.id, TransferStatus::Active, None, 0, attempts)?;

        let result = retry_io(
            || download_to_volume(store, &row.object_key, &dest, entry.size_bytes),
            &mut attempts,
        );

        match result {
            Ok(()) => {
                volume_db.upsert_file(&VolumeFile {
                    track_id: entry.track_id.clone(),
                    relative_path: row.relative_path.clone(),
                    size_bytes: Some(entry.size_bytes),
                    sha256: Some(entry.sha256.clone()),
                    status: VolumeFileStatus::Copied,
                    last_seen_at: Some(Utc::now()),
                })?;
                transfer_db.update_transfer(
                    &row.id,
                    TransferStatus::Done,
                    Some(dest.to_string_lossy().as_ref()),
                    entry.size_bytes,
                    attempts,
                )?;
                done += 1;
            }
            Err(e) => {
                transfer_db.update_transfer(&row.id, TransferStatus::Failed, None, 0, attempts)?;
                volume_db.upsert_file(&VolumeFile {
                    track_id: entry.track_id.clone(),
                    relative_path: row.relative_path.clone(),
                    size_bytes: None,
                    sha256: Some(entry.sha256.clone()),
                    status: VolumeFileStatus::Failed,
                    last_seen_at: Some(Utc::now()),
                })?;
                tracing::warn!("sync transfer failed for {}: {e}", row.relative_path);
                failed += 1;
            }
        }
    }

    if failed == 0 {
        transfer_db.finish_job(job_id)?;
    }
    Ok((done, failed))
}

/// Verify files on a volume against archive entries.
pub fn verify_volume(
    mount: &Path,
    entries: &[ArchiveEntry],
    layout: ExportLayout,
    volume_db: &VolumeDb,
) -> Result<(usize, usize, usize)> {
    let mut present = 0usize;
    let mut missing = 0usize;
    let mut stale = 0usize;
    let mut used_paths: HashSet<String> = HashSet::new();

    for entry in entries {
        let relative_path = unique_destination_path(entry, layout, &mut used_paths);
        let path = mount.join(&relative_path);
        let on_disk = path.is_file()
            && std::fs::metadata(&path)
                .map(|m| m.len() == entry.size_bytes)
                .unwrap_or(false);

        let status = if on_disk {
            present += 1;
            VolumeFileStatus::Copied
        } else if path.exists() {
            stale += 1;
            VolumeFileStatus::Stale
        } else {
            missing += 1;
            VolumeFileStatus::Pending
        };

        volume_db.upsert_file(&VolumeFile {
            track_id: entry.track_id.clone(),
            relative_path,
            size_bytes: if on_disk {
                Some(entry.size_bytes)
            } else {
                None
            },
            sha256: Some(entry.sha256.clone()),
            status,
            last_seen_at: Some(Utc::now()),
        })?;
    }
    Ok((present, missing, stale))
}

/// Diff a track set against volume state without transferring.
pub fn diff_volume(
    entries: &[ArchiveEntry],
    layout: ExportLayout,
    volume_db: &VolumeDb,
    mount: &Path,
) -> Result<Vec<VolumeDiffEntry>> {
    let mut used_paths: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());

    for entry in entries {
        let relative_path = unique_destination_path(entry, layout, &mut used_paths);
        let state = if let Some(existing) = volume_db.get_by_sha256(&entry.sha256)? {
            if existing.status == VolumeFileStatus::Copied
                && file_present_on_disk(Some(mount), &existing.relative_path, entry.size_bytes)
            {
                VolumeDiffState::Present
            } else {
                VolumeDiffState::Stale
            }
        } else if file_present_on_disk(Some(mount), &relative_path, entry.size_bytes) {
            VolumeDiffState::Present
        } else {
            VolumeDiffState::Missing
        };
        out.push(VolumeDiffEntry {
            track_id: entry.track_id.clone(),
            relative_path,
            state,
        });
    }
    Ok(out)
}

/// Render a portable playlist for Mixxx.
pub fn export_playlist(
    playlist_name: &str,
    entries: &[ArchiveEntry],
    layout: ExportLayout,
    format: PlaylistFormat,
    relative_paths: bool,
) -> PlaylistExport {
    let mut used_paths: HashSet<String> = HashSet::new();
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(unique_destination_path(entry, layout, &mut used_paths));
    }

    let ext = match format {
        PlaylistFormat::M3u8 => "m3u8",
        PlaylistFormat::M3u => "m3u",
    };
    let filename = format!("Playlists/{playlist_name}.{ext}");
    let body = render_playlist(&paths, format, relative_paths);
    PlaylistExport {
        filename,
        body,
        relative_paths: paths,
    }
}

/// Write a playlist export file onto a volume.
pub fn write_playlist_export(mount: &Path, export: &PlaylistExport) -> Result<PathBuf> {
    let path = mount.join(&export.filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &export.body)?;
    Ok(path)
}

/// Compute a track's destination path under `Music/` for a given layout.
pub fn destination_path(entry: &ArchiveEntry, layout: ExportLayout) -> String {
    let mut used = HashSet::new();
    unique_destination_path(entry, layout, &mut used)
}

fn unique_destination_path(
    entry: &ArchiveEntry,
    layout: ExportLayout,
    used: &mut HashSet<String>,
) -> String {
    let file_name = display_filename(entry);
    let base = match layout {
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
    };

    if used.insert(base.clone()) {
        return base;
    }

    let (stem, ext) = split_name_ext(&file_name);
    for n in 2..=99 {
        let candidate = match layout {
            ExportLayout::Flat => format!("Music/{stem} ({n}).{ext}"),
            ExportLayout::ArtistAlbum => {
                let artist = entry.metadata.artist.as_deref().unwrap_or("Unknown Artist");
                let album = entry.metadata.album.as_deref().unwrap_or("Unknown Album");
                format!(
                    "Music/{}/{}/{stem} ({n}).{ext}",
                    sanitize(artist),
                    sanitize(album),
                )
            }
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    base
}

/// Human-facing filename for a volume export (never the content hash).
pub fn display_filename(entry: &ArchiveEntry) -> String {
    let ext = extension_for_entry(entry);
    if let Some(title) = entry.metadata.title.as_deref().filter(|t| !t.is_empty()) {
        return format!("{}.{}", sanitize(title), ext);
    }
    if let Some(source) = &entry.source_path_original {
        if let Some(stem) = Path::new(source).file_stem().and_then(|s| s.to_str()) {
            return format!("{}.{}", sanitize(stem), ext);
        }
    }
    format!("track.{}", ext)
}

fn extension_for_entry(entry: &ArchiveEntry) -> String {
    if let Some(ft) = entry.metadata.file_type.as_deref() {
        let ft = ft.trim_start_matches('.');
        if !ft.is_empty() {
            return ft.to_lowercase();
        }
    }
    entry
        .object_key
        .rsplit('.')
        .next()
        .unwrap_or("mp3")
        .to_lowercase()
}

fn render_playlist(paths: &[String], format: PlaylistFormat, relative: bool) -> String {
    let mut lines = Vec::new();
    if matches!(format, PlaylistFormat::M3u8) {
        lines.push("#EXTM3U".to_string());
    }
    for path in paths {
        let line = if relative {
            path.clone()
        } else {
            format!("/{}", path.trim_start_matches('/'))
        };
        lines.push(line);
    }
    lines.join("\n") + "\n"
}

fn download_to_volume(
    store: &dyn ObjectStore,
    object_key: &str,
    dest: &Path,
    expected_size: u64,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = store.get(object_key)?;
    if bytes.len() as u64 != expected_size {
        return Err(Error::Other(format!(
            "size mismatch for {}: expected {expected_size}, got {}",
            dest.display(),
            bytes.len()
        )));
    }
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn file_present_on_disk(mount: Option<&Path>, relative_path: &str, size: u64) -> bool {
    let Some(mount) = mount else {
        return false;
    };
    let path = mount.join(relative_path);
    path.is_file()
        && std::fs::metadata(&path)
            .map(|m| m.len() == size)
            .unwrap_or(false)
}

fn split_name_ext(file_name: &str) -> (&str, &str) {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !ext.is_empty() && !ext.contains('/') => (stem, ext),
        _ => (file_name, "mp3"),
    }
}

fn sanitize(component: &str) -> String {
    component
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

fn retry_io<F>(mut op: F, attempts: &mut u32) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    const BASE_MS: u64 = 500;
    const MAX_MS: u64 = 30_000;

    loop {
        *attempts += 1;
        match op() {
            Ok(()) => return Ok(()),
            Err(e) if *attempts >= MAX_IO_ATTEMPTS || !is_transient(&e) => return Err(e),
            Err(_) => {
                let exp = (*attempts - 1).min(6);
                let delay = (BASE_MS.saturating_mul(1 << exp)).min(MAX_MS);
                thread::sleep(Duration::from_millis(delay + delay / 4));
            }
        }
    }
}

fn is_transient(err: &Error) -> bool {
    matches!(err, Error::Store(_) | Error::Io(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Metadata;
    use chrono::Utc;

    fn sample_entry(title: &str, sha: &str) -> ArchiveEntry {
        let now = Utc::now();
        ArchiveEntry {
            track_id: TrackId::from("t1"),
            object_key: format!("music/{sha}.flac"),
            size_bytes: 100,
            sha256: sha.into(),
            metadata: Metadata {
                title: Some(title.into()),
                artist: Some("Artist".into()),
                album: Some("Album".into()),
                file_type: Some("flac".into()),
                ..Default::default()
            },
            tags: Vec::new(),
            imported_at: now,
            updated_at: now,
            source_path_original: None,
            artwork_object_key: None,
        }
    }

    #[test]
    fn destination_uses_title_not_hash() {
        let entry = sample_entry("Cycles", "abc123");
        let path = destination_path(&entry, ExportLayout::ArtistAlbum);
        assert!(path.ends_with("Cycles.flac"));
        assert!(!path.contains("abc123"));
    }

    #[test]
    fn collision_gets_suffix() {
        let a = sample_entry("Cycles", "sha-a");
        let mut b = sample_entry("Cycles", "sha-b");
        b.track_id = TrackId::from("t2");
        let mut used = HashSet::new();
        let p1 = unique_destination_path(&a, ExportLayout::Flat, &mut used);
        let p2 = unique_destination_path(&b, ExportLayout::Flat, &mut used);
        assert_ne!(p1, p2);
        assert!(p2.contains("(2)"));
    }
}
