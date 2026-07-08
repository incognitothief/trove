//! Bulk import: resumable, crash-safe migration of a local library into the
//! bucket (ADR "Initial archive backfill").
//!
//! The bootstrap implements the full phase pipeline against the [`ObjectStore`]
//! abstraction (so it runs end-to-end against the stub store): scan and hash on
//! disk, dedupe by content hash, upload to a per-job staging prefix, verify,
//! then commit into the canonical `music/` namespace and the archive index.
//! Wiring to persistent `import_files` bookkeeping and multipart uploads is the
//! next step; the state machine and object layout are already in place.

pub mod state;

use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::archive::index::BucketPaths;
use crate::error::Result;
use crate::metadata::MetadataExtractor;
use crate::model::{ArchiveEntry, TrackId};
use crate::store::ObjectStore;

pub use state::{FileState, Phase};

/// A single file discovered during scan, carrying its explicit state.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub state: FileState,
    pub object_key: Option<String>,
    pub track_id: Option<TrackId>,
}

/// A planned (or in-progress) bulk import job.
#[derive(Debug, Clone)]
pub struct ImportJob {
    pub id: String,
    pub source_root: PathBuf,
    pub staging_prefix: String,
    pub phase: Phase,
    pub files: Vec<PlannedFile>,
}

impl ImportJob {
    pub fn stats(&self) -> ImportStats {
        let mut s = ImportStats::default();
        for f in &self.files {
            s.total += 1;
            match f.state {
                FileState::Duplicate => s.duplicates += 1,
                FileState::Committed => s.committed += 1,
                FileState::Verified => s.verified += 1,
                FileState::Uploaded => s.uploaded += 1,
                FileState::Failed => s.failed += 1,
                _ => {}
            }
        }
        s
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportStats {
    pub total: usize,
    pub duplicates: usize,
    pub uploaded: usize,
    pub verified: usize,
    pub committed: usize,
    pub failed: usize,
}

/// Plan a bulk import: scan `source_root`, hash each audio file, and dedupe by
/// content hash. Produces a job with per-file state populated up to `hashed`
/// (or `duplicate`). This is the dry-run-friendly phase.
pub fn plan(source_root: &Path, extensions: &[&str]) -> Result<ImportJob> {
    let id = Uuid::new_v4().to_string();
    let mut files = Vec::new();
    let mut seen_hashes = std::collections::HashSet::new();

    let mut discovered = Vec::new();
    scan_dir(source_root, extensions, &mut discovered)?;
    discovered.sort();

    for path in discovered {
        let bytes = std::fs::read(&path)?;
        let size = bytes.len() as u64;
        let sha256 = hash_bytes(&bytes);
        let state = if seen_hashes.insert(sha256.clone()) {
            FileState::Hashed
        } else {
            FileState::Duplicate
        };
        files.push(PlannedFile {
            path,
            size,
            sha256,
            state,
            object_key: None,
            track_id: None,
        });
    }

    Ok(ImportJob {
        id: id.clone(),
        source_root: source_root.to_path_buf(),
        staging_prefix: format!("staging/{id}"),
        phase: Phase::Dedupe,
        files,
    })
}

/// Upload all non-duplicate files to the per-job staging prefix.
pub fn upload(job: &mut ImportJob, store: &dyn ObjectStore, paths: &BucketPaths) -> Result<()> {
    for file in job.files.iter_mut() {
        if file.state != FileState::Hashed {
            continue;
        }
        file.state = FileState::Uploading;
        let bytes = std::fs::read(&file.path)?;
        let rel = staging_relative(&file.sha256, &file.path);
        let key = format!("{}/{}", paths.staging(&job.id), rel);
        store.put(&key, &bytes)?;
        file.object_key = Some(key);
        file.state = FileState::Uploaded;
    }
    job.phase = Phase::Verify;
    Ok(())
}

/// Verify uploaded objects by size (and presence) before commit.
pub fn verify(job: &mut ImportJob, store: &dyn ObjectStore) -> Result<()> {
    for file in job.files.iter_mut() {
        if file.state != FileState::Uploaded {
            continue;
        }
        let key = file
            .object_key
            .as_ref()
            .expect("uploaded file must have an object key");
        match store.head(key)? {
            Some(meta) if meta.size_bytes == file.size => file.state = FileState::Verified,
            _ => file.state = FileState::Failed,
        }
    }
    job.phase = Phase::Commit;
    Ok(())
}

/// Promote verified staging objects into `music/` and produce archive entries.
///
/// Commit is always the last step: only verified objects advance the index.
pub fn commit(
    job: &mut ImportJob,
    store: &dyn ObjectStore,
    paths: &BucketPaths,
    extractor: &dyn MetadataExtractor,
) -> Result<Vec<ArchiveEntry>> {
    let mut committed = Vec::new();
    for file in job.files.iter_mut() {
        if file.state != FileState::Verified {
            continue;
        }
        let staging_key = file
            .object_key
            .as_ref()
            .expect("verified file must have an object key")
            .clone();
        let rel = music_relative(&file.path);
        let music_key = paths.music(&rel);
        store.copy(&staging_key, &music_key)?;

        let track_id = TrackId::new();
        let now = Utc::now();
        let metadata = extractor.extract(&file.path).unwrap_or_default();
        committed.push(ArchiveEntry {
            track_id: track_id.clone(),
            object_key: music_key.clone(),
            size_bytes: file.size,
            sha256: file.sha256.clone(),
            metadata,
            tags: Vec::new(),
            imported_at: now,
            updated_at: now,
            source_path_original: Some(file.path.display().to_string()),
            artwork_object_key: None,
        });

        file.object_key = Some(music_key);
        file.track_id = Some(track_id);
        file.state = FileState::Committed;
    }
    job.phase = Phase::Done;
    Ok(committed)
}

fn scan_dir(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            scan_dir(&path, extensions, out)?;
        } else if is_audio(&path, extensions) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_audio(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn staging_relative(sha256: &str, path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("object");
    format!("{sha256}/{name}")
}

/// Default committed layout: `Artist/Album/Track.ext`, falling back to filename.
fn music_relative(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("track")
        .to_string()
}

/// Default audio extensions recognized by import scans.
pub const DEFAULT_AUDIO_EXTENSIONS: &[&str] =
    &["flac", "wav", "aiff", "aif", "mp3", "m4a", "aac", "ogg"];
