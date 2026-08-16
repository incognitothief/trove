//! Bulk import: resumable, crash-safe migration of a local library into the
//! bucket (ADR "Initial archive backfill").
//!
//! Import state persists to `~/.trove/sync.sqlite` (ADR 005) so a crash mid-job
//! resumes without rescanning or rehashing finished files. Canonical audio keys
//! are content-addressed (`music/<sha256>.<ext>`, ADR 004).

pub mod progress;
pub mod state;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::archive::index::BucketPaths;
use crate::db::archive::ArchiveDb;
use crate::db::import::ImportDb;
use crate::error::{Error, Result};
use crate::metadata::MetadataExtractor;
use crate::model::{ArchiveEntry, ArtworkRecord, TrackId};
use crate::store::ObjectStore;

pub use progress::{
    ImportProgress, ImportProgressEvent, ImportProgressKind, NoopImportProgress, ProgressCtx,
};
pub use state::{FileState, Phase};

/// Options controlling scan-time import behavior (ADR 002).
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Include dotfiles / hidden directories (default: exclude them).
    pub include_dotfiles: bool,
    /// Capture co-located cover art for directory imports (default: on).
    pub capture_artwork: bool,
    /// Explicit cover-art image files or folders (`--artwork PATH`).
    pub artwork_paths: Vec<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            include_dotfiles: false,
            capture_artwork: true,
            artwork_paths: Vec::new(),
        }
    }
}

/// A single file discovered during scan, carrying its explicit state.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: Option<String>,
    pub sha256: String,
    pub state: FileState,
    pub object_key: Option<String>,
    pub track_id: Option<TrackId>,
    pub etag: Option<String>,
    pub error: Option<String>,
    pub attempts: u32,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportStats {
    pub total: usize,
    pub duplicates: usize,
    pub uploaded: usize,
    pub verified: usize,
    pub committed: usize,
    pub failed: usize,
}

/// Machine-readable import manifest written under `~/.trove/cache/manifests/`.
#[derive(Debug, Serialize)]
pub struct ImportManifest {
    pub job_id: String,
    pub source_root: PathBuf,
    pub phase: Phase,
    pub files: Vec<ImportManifestFile>,
    pub written_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ImportManifestFile {
    pub path: PathBuf,
    pub sha256: String,
    pub state: FileState,
    pub object_key: Option<String>,
}

const MAX_STORE_ATTEMPTS: u32 = 5;

/// Plan a bulk import: scan `source_root`, hash each audio file, dedupe by
/// content hash, and (per options) gather co-located cover art.
///
/// When `db` is provided, the job shell and per-file rows are persisted
/// incrementally so an interrupt during fingerprinting remains resumable.
/// Pass `resume_job_id` to continue an in-flight plan.
pub fn plan(
    source_root: &Path,
    extensions: &[&str],
    options: &ImportOptions,
    archive: Option<&ArchiveDb>,
    db: Option<&ImportDb>,
    resume_job_id: Option<&str>,
    progress: &mut dyn ImportProgress,
) -> Result<ImportJob> {
    let (id, staging_prefix, source_root, mut existing) = if let Some(job_id) = resume_job_id {
        let (job, _) = db
            .ok_or_else(|| Error::Other("resume requires ImportDb".into()))?
            .load_job(job_id)?;
        let mut by_path = std::collections::HashMap::new();
        for file in job.files {
            by_path.insert(file.path.clone(), file);
        }
        (job.id, job.staging_prefix, job.source_root, by_path)
    } else {
        let id = Uuid::new_v4().to_string();
        let staging_prefix = format!("staging/{id}");
        if let Some(db) = db {
            db.create_job_shell(&id, source_root, &staging_prefix, options)?;
        }
        (
            id.clone(),
            staging_prefix,
            source_root.to_path_buf(),
            std::collections::HashMap::new(),
        )
    };

    let mut progress = progress::ProgressCtx::new(&id, Some(progress));
    if resume_job_id.is_none() {
        progress.job_created();
    }

    let mut discovered = Vec::new();
    discover_audio(
        &source_root,
        extensions,
        options.include_dotfiles,
        &mut discovered,
    )?;
    discovered.sort();

    let total = discovered.len();
    if let Some(db) = db {
        db.set_total_files(&id, total)?;
        db.insert_pending_files(&id, &discovered)?;
        db.update_phase(&id, Phase::Fingerprint)?;
    }

    let mut seen_hashes: std::collections::HashSet<String> = existing
        .values()
        .filter(|f| matches!(f.state, FileState::Hashed | FileState::Duplicate))
        .map(|f| f.sha256.clone())
        .collect();

    progress.phase_start(Phase::Fingerprint, total);

    let mut files = Vec::with_capacity(total);
    let mut done = 0usize;
    for path in &discovered {
        let file = if let Some(existing) = existing.remove(path) {
            if can_skip_fingerprint(&existing, path)? {
                done += 1;
                progress.file_done(Phase::Fingerprint, done, total, path, existing.state);
                files.push(existing);
                continue;
            }
            existing
        } else {
            PlannedFile {
                path: path.clone(),
                size: 0,
                mtime: None,
                sha256: String::new(),
                state: FileState::Pending,
                object_key: None,
                track_id: None,
                etag: None,
                error: None,
                attempts: 0,
            }
        };

        let mut file = file;
        file.state = FileState::Scanning;
        persist_file(db, &id, &file)?;

        let meta = std::fs::metadata(path)?;
        file.size = meta.len();
        file.mtime = file_mtime_from_meta(&meta);
        let bytes = std::fs::read(path)?;
        file.sha256 = hash_bytes(&bytes);
        let already_in_archive = match archive {
            Some(db) => db.find_by_sha256(&file.sha256)?.is_some(),
            None => false,
        };
        file.state = if !seen_hashes.insert(file.sha256.clone()) || already_in_archive {
            FileState::Duplicate
        } else {
            FileState::Hashed
        };
        file.error = None;
        persist_file(db, &id, &file)?;

        done += 1;
        progress.file_done(Phase::Fingerprint, done, total, path, file.state);
        files.push(file);
    }

    progress.phase_done(Phase::Dedupe, done, total);

    // Directory imports gather co-located artwork by default. Explicit paths
    // (`--artwork PATH`) add image files or scan folders you name directly.
    let mut artwork = if options.capture_artwork && source_root.is_dir() {
        gather_artwork(&discovered, options.include_dotfiles)?
    } else {
        Vec::new()
    };
    if !options.artwork_paths.is_empty() {
        merge_artwork(
            &mut artwork,
            gather_artwork_from_paths(&options.artwork_paths, options.include_dotfiles)?,
        );
    }

    if let Some(db) = db {
        db.update_phase(&id, Phase::Dedupe)?;
        db.save_artwork(&id, &artwork)?;
    }

    Ok(ImportJob {
        id,
        source_root,
        staging_prefix,
        phase: Phase::Dedupe,
        files,
        artwork,
    })
}

/// Whether a previously fingerprinted file can be skipped on resume.
fn can_skip_fingerprint(file: &PlannedFile, path: &Path) -> Result<bool> {
    if !matches!(file.state, FileState::Hashed | FileState::Duplicate) {
        return Ok(false);
    }
    if file.sha256.is_empty() {
        return Ok(false);
    }
    let meta = std::fs::metadata(path)?;
    Ok(file.size == meta.len() && file.mtime == file_mtime_from_meta(&meta))
}

/// Reset incomplete upload states so resume can retry safely.
pub fn prepare_for_resume(job: &mut ImportJob) {
    for file in job.files.iter_mut() {
        match file.state {
            FileState::Uploading | FileState::Failed => {
                file.state = FileState::Hashed;
                file.error = None;
            }
            _ => {}
        }
    }
}

/// Re-hash files whose on-disk size/mtime changed since the last plan.
pub fn refresh_changed_files(job: &mut ImportJob) -> Result<()> {
    for file in job.files.iter_mut() {
        if file.state == FileState::Duplicate || file.state == FileState::Committed {
            continue;
        }
        if file.sha256.is_empty() {
            continue;
        }
        let meta = match std::fs::metadata(&file.path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = meta.len();
        let mtime = file_mtime_from_meta(&meta);
        if file.size == size && file.mtime == mtime {
            continue;
        }
        let bytes = std::fs::read(&file.path)?;
        file.size = size;
        file.mtime = mtime;
        file.sha256 = hash_bytes(&bytes);
        file.state = FileState::Hashed;
        file.object_key = None;
        file.etag = None;
        file.error = None;
        file.attempts = 0;
    }
    Ok(())
}

/// Collect co-located cover-art candidates next to discovered audio.
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
        gather_artwork_in_folder(&folder, include_dotfiles, &mut candidates, &mut seen)?;
    }
    Ok(candidates)
}

/// Collect cover-art from explicit `--artwork` paths (image file or folder).
fn gather_artwork_from_paths(
    paths: &[PathBuf],
    include_dotfiles: bool,
) -> Result<Vec<ArtworkCandidate>> {
    let mut candidates = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for path in paths {
        if !path.exists() {
            return Err(Error::Other(format!(
                "artwork path not found: {}",
                path.display()
            )));
        }
        if path.is_file() {
            if is_hidden(path, include_dotfiles) {
                continue;
            }
            if !is_image(path, DEFAULT_IMAGE_EXTENSIONS) {
                return Err(Error::Other(format!(
                    "artwork path is not an image: {}",
                    path.display()
                )));
            }
            push_artwork_file(path, path.parent(), &mut candidates, &mut seen)?;
        } else if path.is_dir() {
            gather_artwork_in_folder(path, include_dotfiles, &mut candidates, &mut seen)?;
        } else {
            return Err(Error::Other(format!(
                "artwork path is not a file or directory: {}",
                path.display()
            )));
        }
    }
    Ok(candidates)
}

fn gather_artwork_in_folder(
    folder: &Path,
    include_dotfiles: bool,
    candidates: &mut Vec<ArtworkCandidate>,
    seen: &mut std::collections::HashSet<(String, String)>,
) -> Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if !path.is_file() || is_hidden(&path, include_dotfiles) {
            continue;
        }
        if !is_image(&path, DEFAULT_IMAGE_EXTENSIONS) {
            continue;
        }
        push_artwork_file(&path, Some(folder), candidates, seen)?;
    }
    Ok(())
}

fn push_artwork_file(
    path: &Path,
    source_folder: Option<&Path>,
    candidates: &mut Vec<ArtworkCandidate>,
    seen: &mut std::collections::HashSet<(String, String)>,
) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let sha256 = hash_bytes(&bytes);
    let source_folder = source_folder
        .unwrap_or_else(|| path.parent().unwrap_or(Path::new("")))
        .display()
        .to_string();
    if !seen.insert((sha256.clone(), source_folder.clone())) {
        return Ok(());
    }
    candidates.push(ArtworkCandidate {
        path: path.to_path_buf(),
        sha256,
        size: bytes.len() as u64,
        source_folder,
        object_key: None,
    });
    Ok(())
}

fn merge_artwork(into: &mut Vec<ArtworkCandidate>, more: Vec<ArtworkCandidate>) {
    let mut seen: std::collections::HashSet<(String, String)> = into
        .iter()
        .map(|c| (c.sha256.clone(), c.source_folder.clone()))
        .collect();
    for candidate in more {
        if seen.insert((candidate.sha256.clone(), candidate.source_folder.clone())) {
            into.push(candidate);
        }
    }
}

/// Capture artwork candidates into the content-addressed `artwork/` namespace.
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
pub fn upload(
    job: &mut ImportJob,
    store: &dyn ObjectStore,
    paths: &BucketPaths,
    db: Option<&ImportDb>,
    progress: &mut dyn ImportProgress,
) -> Result<()> {
    job.phase = Phase::Upload;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }

    let mut progress = progress::ProgressCtx::new(&job.id, Some(progress));

    let pending = job
        .files
        .iter()
        .filter(|f| matches!(f.state, FileState::Hashed))
        .count();
    progress.phase_start(Phase::Upload, pending);

    let mut done = 0usize;
    for file in job.files.iter_mut() {
        if !matches!(file.state, FileState::Hashed) {
            continue;
        }
        file.state = FileState::Uploading;
        persist_file(db, &job.id, file)?;

        let bytes = std::fs::read(&file.path)?;
        let rel = staging_relative(&file.sha256, &file.path);
        let key = format!("{}/{}", paths.staging(&job.id), rel);

        let meta = retry_store(|| store.put(&key, &bytes), &mut file.attempts);
        match meta {
            Ok(meta) => {
                file.object_key = Some(meta.key);
                file.etag = meta.etag;
                file.state = FileState::Uploaded;
                file.error = None;
            }
            Err(e) => {
                file.state = FileState::Failed;
                file.error = Some(e.to_string());
            }
        }
        persist_file(db, &job.id, file)?;
        done += 1;
        progress.file_done(Phase::Upload, done, pending, &file.path, file.state);
    }

    progress.phase_done(Phase::Upload, done, pending);

    job.phase = Phase::Verify;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }
    Ok(())
}

/// Verify uploaded objects by size (and presence) before commit, or — when
/// `deep` is set — by re-downloading and re-hashing each object and
/// comparing against the SHA-256 computed at fingerprint time. Size/presence
/// alone (the default) is cheap but weak: a truncated or bit-flipped upload
/// with the right byte count would pass. `deep` actually verifies the
/// content-addressed identity ADR 004 relies on, at the cost of re-reading
/// every byte — expensive, and meant to be opted into explicitly (ADR 007,
/// Group B2), not run by default on every `import run`.
pub fn verify(
    job: &mut ImportJob,
    store: &dyn ObjectStore,
    db: Option<&ImportDb>,
    deep: bool,
    progress: &mut dyn ImportProgress,
) -> Result<()> {
    job.phase = Phase::Verify;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }

    let mut progress = progress::ProgressCtx::new(&job.id, Some(progress));

    let pending = job
        .files
        .iter()
        .filter(|f| f.state == FileState::Uploaded)
        .count();
    progress.phase_start(Phase::Verify, pending);

    let mut done = 0usize;
    for file in job.files.iter_mut() {
        if file.state != FileState::Uploaded {
            continue;
        }
        let key = match file.object_key.as_ref() {
            Some(k) => k.clone(),
            None => {
                file.state = FileState::Failed;
                file.error = Some("uploaded file missing object key".into());
                persist_file(db, &job.id, file)?;
                done += 1;
                progress.file_done(Phase::Verify, done, pending, &file.path, file.state);
                continue;
            }
        };

        let expected_sha = file.sha256.clone();
        let verify_result = retry_store(
            || match store.head(&key)? {
                Some(meta) if meta.size_bytes != file.size => Err(Error::store(format!(
                    "size mismatch for {key}: expected {} got {}",
                    file.size, meta.size_bytes
                ))),
                Some(_) if deep => {
                    let bytes = store.get(&key)?;
                    let actual_sha = hash_bytes(&bytes);
                    if actual_sha == expected_sha {
                        Ok(())
                    } else {
                        Err(Error::store(format!(
                            "content hash mismatch for {key}: expected {expected_sha} got {actual_sha}"
                        )))
                    }
                }
                Some(_) => Ok(()),
                None => Err(Error::not_found(key.clone())),
            },
            &mut file.attempts,
        );

        match verify_result {
            Ok(()) => {
                file.state = FileState::Verified;
                file.error = None;
            }
            Err(e) => {
                file.state = FileState::Failed;
                file.error = Some(e.to_string());
            }
        }
        persist_file(db, &job.id, file)?;
        done += 1;
        progress.file_done(Phase::Verify, done, pending, &file.path, file.state);
    }

    progress.phase_done(Phase::Verify, done, pending);

    job.phase = Phase::Commit;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }
    Ok(())
}

/// Promote verified staging objects into `music/` and produce archive entries.
pub fn commit(
    job: &mut ImportJob,
    store: &dyn ObjectStore,
    paths: &BucketPaths,
    archive: &ArchiveDb,
    extractor: &dyn MetadataExtractor,
    db: Option<&ImportDb>,
    progress: &mut dyn ImportProgress,
) -> Result<Vec<ArchiveEntry>> {
    job.phase = Phase::Commit;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }

    let mut progress = progress::ProgressCtx::new(&job.id, Some(progress));

    let pending = job
        .files
        .iter()
        .filter(|f| f.state == FileState::Verified)
        .count();
    progress.phase_start(Phase::Commit, pending);

    let mut committed = Vec::new();
    let mut done = 0usize;
    for file in job.files.iter_mut() {
        if file.state != FileState::Verified {
            continue;
        }

        if let Some(existing) = archive.find_by_sha256(&file.sha256)? {
            file.object_key = Some(existing.object_key.clone());
            file.track_id = Some(existing.track_id.clone());
            file.state = FileState::Duplicate;
            persist_file(db, &job.id, file)?;
            done += 1;
            progress.file_done(Phase::Commit, done, pending, &file.path, file.state);
            continue;
        }

        let staging_key = file
            .object_key
            .as_ref()
            .expect("verified file must have an object key")
            .clone();
        let rel = canonical_music_relative(&file.sha256, &file.path);
        let music_key = paths.music(&rel);

        let copy_result = retry_store(
            || {
                if !store.exists(&music_key)? {
                    store.copy(&staging_key, &music_key)?;
                }
                Ok(())
            },
            &mut file.attempts,
        );

        if let Err(e) = copy_result {
            file.state = FileState::Failed;
            file.error = Some(e.to_string());
            persist_file(db, &job.id, file)?;
            done += 1;
            progress.file_done(Phase::Commit, done, pending, &file.path, file.state);
            continue;
        }

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
        persist_file(db, &job.id, file)?;
        done += 1;
        progress.file_done(Phase::Commit, done, pending, &file.path, file.state);
    }

    progress.phase_done(Phase::Commit, done, pending);

    job.phase = Phase::Done;
    if let Some(db) = db {
        db.update_phase(&job.id, job.phase)?;
    }
    Ok(committed)
}

/// Write a machine-readable manifest for operator inspection.
pub fn write_manifest(home: &Path, job: &ImportJob) -> Result<PathBuf> {
    let dir = home.join("cache/manifests");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", job.id));
    let manifest = ImportManifest {
        job_id: job.id.clone(),
        source_root: job.source_root.clone(),
        phase: job.phase,
        files: job
            .files
            .iter()
            .map(|f| ImportManifestFile {
                path: f.path.clone(),
                sha256: f.sha256.clone(),
                state: f.state,
                object_key: f.object_key.clone(),
            })
            .collect(),
        written_at: Utc::now(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Remove the local manifest for a pruned import job.
pub fn remove_manifest(home: &Path, job_id: &str) -> Result<()> {
    let path = home.join("cache/manifests").join(format!("{job_id}.json"));
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn persist_file(db: Option<&ImportDb>, job_id: &str, file: &PlannedFile) -> Result<()> {
    if let Some(db) = db {
        db.update_file(job_id, file)?;
    }
    Ok(())
}

fn retry_store<F, T>(mut op: F, attempts: &mut u32) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    const BASE_MS: u64 = 500;
    const MAX_MS: u64 = 30_000;

    loop {
        *attempts += 1;
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if *attempts >= MAX_STORE_ATTEMPTS || !is_transient(&e) => return Err(e),
            Err(_) => {
                let exp = (*attempts - 1).min(6);
                let delay = (BASE_MS.saturating_mul(1 << exp)).min(MAX_MS);
                let jitter = delay / 4;
                thread::sleep(Duration::from_millis(delay + jitter));
            }
        }
    }
}

fn is_transient(err: &Error) -> bool {
    matches!(err, Error::Store(_) | Error::Io(_))
}

fn file_mtime_from_meta(meta: &std::fs::Metadata) -> Option<String> {
    meta.modified().ok().map(format_mtime)
}

fn format_mtime(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
}

/// Discover importable audio under `source`, which may be a directory or a single file.
fn discover_audio(
    source: &Path,
    extensions: &[&str],
    include_dotfiles: bool,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if source.is_dir() {
        scan_dir(source, extensions, include_dotfiles, out)
    } else if source.is_file() {
        if is_hidden(source, include_dotfiles) {
            return Ok(());
        }
        if is_audio(source, extensions) {
            out.push(source.to_path_buf());
        }
        Ok(())
    } else {
        Err(Error::not_found(format!(
            "import source not found: {}",
            source.display()
        )))
    }
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

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
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

/// Canonical committed layout: `<sha256>.<ext>` under `music/` (ADR 004).
pub fn canonical_music_relative(sha256: &str, path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "bin".to_string());
    format!("{sha256}.{ext}")
}

/// Default audio extensions recognized by import scans (the ingest allowlist).
pub const DEFAULT_AUDIO_EXTENSIONS: &[&str] =
    &["flac", "wav", "aiff", "aif", "mp3", "m4a", "aac", "ogg"];

/// Image extensions treated as candidate cover art when co-located with audio.
pub const DEFAULT_IMAGE_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_music_relative_uses_hash_and_extension() {
        let path = Path::new("/music/Artist/Album/01 - Intro.mp3");
        assert_eq!(canonical_music_relative("abc123", path), "abc123.mp3");
    }

    #[test]
    fn prepare_for_resume_resets_uploading_and_failed() {
        let mut job = ImportJob {
            id: "j".into(),
            source_root: PathBuf::from("/src"),
            staging_prefix: "staging/j".into(),
            phase: Phase::Upload,
            files: vec![
                PlannedFile {
                    path: PathBuf::from("/src/a.mp3"),
                    size: 1,
                    mtime: None,
                    sha256: "a".into(),
                    state: FileState::Uploading,
                    object_key: None,
                    track_id: None,
                    etag: None,
                    error: Some("timeout".into()),
                    attempts: 2,
                },
                PlannedFile {
                    path: PathBuf::from("/src/b.mp3"),
                    size: 1,
                    mtime: None,
                    sha256: "b".into(),
                    state: FileState::Uploaded,
                    object_key: Some("k".into()),
                    track_id: None,
                    etag: None,
                    error: None,
                    attempts: 0,
                },
            ],
            artwork: Vec::new(),
        };
        prepare_for_resume(&mut job);
        assert_eq!(job.files[0].state, FileState::Hashed);
        assert!(job.files[0].error.is_none());
        assert_eq!(job.files[1].state, FileState::Uploaded);
    }
}
