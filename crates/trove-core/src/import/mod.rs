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
use crate::model::{ArchiveEntry, ArtworkRecord, TrackId};
use crate::store::ObjectStore;

pub use state::{FileState, Phase};

/// Options controlling scan-time import behavior (ADR 002).
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Include dotfiles / hidden directories (default: exclude them).
    pub include_dotfiles: bool,
    /// Capture co-located cover art (default: on — a durability measure).
    pub capture_artwork: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            include_dotfiles: false,
            capture_artwork: true,
        }
    }
}

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

/// A cover-art image found next to imported audio, captured for durability.
#[derive(Debug, Clone)]
pub struct ArtworkCandidate {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    /// Source folder the image was found in (provenance for later association).
    pub source_folder: String,
    pub object_key: Option<String>,
}

/// A planned (or in-progress) bulk import job.
#[derive(Debug, Clone)]
pub struct ImportJob {
    pub id: String,
    pub source_root: PathBuf,
    pub staging_prefix: String,
    pub phase: Phase,
    pub files: Vec<PlannedFile>,
    /// Co-located cover-art candidates captured during scan.
    pub artwork: Vec<ArtworkCandidate>,
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

/// Plan a bulk import: scan `source_root`, hash each audio file, dedupe by
/// content hash, and (per options) gather co-located cover art. Produces a job
/// with per-file state populated up to `hashed` (or `duplicate`). This is the
/// dry-run-friendly phase.
pub fn plan(source_root: &Path, extensions: &[&str], options: &ImportOptions) -> Result<ImportJob> {
    let id = Uuid::new_v4().to_string();
    let mut files = Vec::new();
    let mut seen_hashes = std::collections::HashSet::new();

    let mut discovered = Vec::new();
    scan_dir(
        source_root,
        extensions,
        options.include_dotfiles,
        &mut discovered,
    )?;
    discovered.sort();

    for path in &discovered {
        let bytes = std::fs::read(path)?;
        let size = bytes.len() as u64;
        let sha256 = hash_bytes(&bytes);
        let state = if seen_hashes.insert(sha256.clone()) {
            FileState::Hashed
        } else {
            FileState::Duplicate
        };
        files.push(PlannedFile {
            path: path.clone(),
            size,
            sha256,
            state,
            object_key: None,
            track_id: None,
        });
    }

    let artwork = if options.capture_artwork {
        gather_artwork(&discovered, options.include_dotfiles)?
    } else {
        Vec::new()
    };

    Ok(ImportJob {
        id: id.clone(),
        source_root: source_root.to_path_buf(),
        staging_prefix: format!("staging/{id}"),
        phase: Phase::Dedupe,
        files,
        artwork,
    })
}

/// Collect co-located cover-art candidates: image files sharing a folder with at
/// least one imported audio file. Content-addressed so identical covers dedupe.
fn gather_artwork(
    audio_paths: &[PathBuf],
    include_dotfiles: bool,
) -> Result<Vec<ArtworkCandidate>> {
    let mut folders: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for path in audio_paths {
        if let Some(parent) = path.parent() {
            folders.insert(parent.to_path_buf());
        }
    }

    let mut candidates = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for folder in folders {
        for entry in std::fs::read_dir(&folder)? {
            let path = entry?.path();
            if !path.is_file() || is_hidden(&path, include_dotfiles) {
                continue;
            }
            if !is_image(&path, DEFAULT_IMAGE_EXTENSIONS) {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let sha256 = hash_bytes(&bytes);
            let source_folder = folder.display().to_string();
            if !seen.insert((sha256.clone(), source_folder.clone())) {
                continue;
            }
            candidates.push(ArtworkCandidate {
                path: path.clone(),
                sha256,
                size: bytes.len() as u64,
                source_folder,
                object_key: None,
            });
        }
    }
    Ok(candidates)
}

/// Capture artwork candidates into the content-addressed `artwork/` namespace
/// and return provenance records for the durable manifest. Idempotent: an
/// already-present art object is not re-uploaded.
pub fn capture_artwork(
    job: &mut ImportJob,
    store: &dyn ObjectStore,
    paths: &BucketPaths,
) -> Result<Vec<ArtworkRecord>> {
    let mut records = Vec::new();
    for art in job.artwork.iter_mut() {
        let ext = art
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_else(|| "img".to_string());
        let key = paths.artwork(&format!("{}.{}", art.sha256, ext));
        if !store.exists(&key)? {
            let bytes = std::fs::read(&art.path)?;
            store.put(&key, &bytes)?;
        }
        art.object_key = Some(key.clone());
        records.push(ArtworkRecord {
            sha256: art.sha256.clone(),
            object_key: key,
            size_bytes: art.size,
            source_folder: art.source_folder.clone(),
            file_name: art
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("cover")
                .to_string(),
            captured_at: Utc::now(),
        });
    }
    Ok(records)
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

fn scan_dir(
    dir: &Path,
    extensions: &[&str],
    include_dotfiles: bool,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_hidden(&path, include_dotfiles) {
            continue;
        }
        if path.is_dir() {
            scan_dir(&path, extensions, include_dotfiles, out)?;
        } else if is_audio(&path, extensions) {
            out.push(path);
        }
    }
    Ok(())
}

/// Whether a path should be skipped as hidden. Always false when dotfiles are
/// explicitly included.
fn is_hidden(path: &Path, include_dotfiles: bool) -> bool {
    if include_dotfiles {
        return false;
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

fn is_audio(path: &Path, extensions: &[&str]) -> bool {
    has_extension(path, extensions)
}

fn is_image(path: &Path, extensions: &[&str]) -> bool {
    has_extension(path, extensions)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
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

/// Default audio extensions recognized by import scans (the ingest allowlist).
pub const DEFAULT_AUDIO_EXTENSIONS: &[&str] =
    &["flac", "wav", "aiff", "aif", "mp3", "m4a", "aac", "ogg"];

/// Image extensions treated as candidate cover art when co-located with audio.
pub const DEFAULT_IMAGE_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif"];
