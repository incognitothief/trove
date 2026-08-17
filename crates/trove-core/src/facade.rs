//! The `Trove` facade — the single entry point thin clients drive.
//!
//! `trove-cli` and `trove-serverd` (and therefore the React UI) hold no logic
//! of their own; they construct a [`Trove`] and call these methods. The facade
//! never lets a client touch S3, SQLite, or volumes directly.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::archive::index::{self, BucketPaths};
use crate::archive::reconcile::{reconcile, ReconcileReport};
use crate::archive::CURRENT_SCHEMA_VERSION;
use crate::config::Config;
use crate::db::archive::ArchiveDb;
use crate::db::fingerprint::FingerprintCache;
use crate::db::import::{ImportDb, ImportJobSummary, ImportStatusReport};
use crate::db::transfer::TransferDb;
use crate::db::volume::VolumeFileStats;
use crate::error::Result;
use crate::import::ImportProgress;
use crate::import::{self, ImportJob, ImportOptions, Phase};
use crate::metadata::{MetadataExtractor, StubExtractor};
use crate::model::SchemaVersion;
use crate::model::{ArchiveEntry, ArtworkRecord, Playlist, TrackId};
use crate::playlist::PlaylistDb;
use crate::query::QuerySpec;
use crate::store::{stub::StubStore, ObjectStore, PutOutcome};
use crate::sync::{self, PlaylistExport, SyncPlan, VolumeDiffEntry};
use crate::volume;

/// Aggregates configuration, local databases, and the durable object store.
pub struct Trove {
    pub config: Config,
    pub home: PathBuf,
    pub paths: BucketPaths,
    archive: ArchiveDb,
    playlists: PlaylistDb,
    import_db: ImportDb,
    transfer_db: TransferDb,
    fingerprint_cache: FingerprintCache,
    store: Box<dyn ObjectStore>,
    extractor: Box<dyn MetadataExtractor>,
}

impl Trove {
    /// Open Trove against a specific home directory and object store.
    pub fn open_with_store(
        config: Config,
        home: &Path,
        store: Box<dyn ObjectStore>,
    ) -> Result<Self> {
        let paths = BucketPaths::new(
            config.bucket.prefix.clone(),
            config.bucket.music_prefix.clone(),
        );
        let archive = ArchiveDb::open(&home.join("archive.sqlite"))?;
        let playlists = PlaylistDb::open(&home.join("playlists.sqlite"))?;
        let import_db = ImportDb::open(&home.join("sync.sqlite"))?;
        let transfer_db = TransferDb::open(&home.join("sync.sqlite"))?;
        let fingerprint_cache = FingerprintCache::open(&home.join("fingerprint_cache.sqlite"))?;
        Ok(Trove {
            config,
            home: home.to_path_buf(),
            paths,
            archive,
            playlists,
            import_db,
            transfer_db,
            fingerprint_cache,
            store,
            extractor: Box::new(StubExtractor),
        })
    }

    /// Open Trove with fully in-memory state and a stub store (tests/bootstrap).
    pub fn in_memory(config: Config) -> Result<Self> {
        Self::in_memory_with_store(config, Box::new(StubStore::new()))
    }

    /// Open Trove with in-memory databases over a caller-provided store.
    pub fn in_memory_with_store(config: Config, store: Box<dyn ObjectStore>) -> Result<Self> {
        let paths = BucketPaths::new(
            config.bucket.prefix.clone(),
            config.bucket.music_prefix.clone(),
        );
        Ok(Trove {
            config,
            home: PathBuf::from(":memory:"),
            paths,
            archive: ArchiveDb::in_memory()?,
            playlists: PlaylistDb::in_memory()?,
            import_db: ImportDb::in_memory()?,
            transfer_db: TransferDb::in_memory()?,
            fingerprint_cache: FingerprintCache::in_memory()?,
            store,
            extractor: Box::new(StubExtractor),
        })
    }

    /// Reconcile the local cache against the bucket (the read precondition).
    pub fn reconcile(&mut self, allow_offline: bool) -> Result<ReconcileReport> {
        reconcile(
            self.store.as_ref(),
            &self.paths,
            &mut self.archive,
            allow_offline,
        )
    }

    /// Reconcile, then run a query against the freshly reconciled cache.
    pub fn query(&mut self, spec: &QuerySpec, allow_offline: bool) -> Result<Vec<ArchiveEntry>> {
        self.reconcile(allow_offline)?;
        self.archive.query(spec)
    }

    /// Fetch a single entry by id (no implicit reconcile).
    pub fn get(&self, id: &TrackId) -> Result<Option<ArchiveEntry>> {
        self.archive.get(id)
    }

    /// The declared library root, if one has been set (ADR 007, Group D1) —
    /// the stable anchor `library_relative_path` (Group D2), the shape scan
    /// (Group E1), and the backfill Plan (Group E2) all compute portable
    /// identity relative to, instead of whatever path a given command's own
    /// arguments happened to be pointed at.
    pub fn library_root(&self) -> Option<&Path> {
        self.config.library.root.as_deref()
    }

    /// Declare (or change) the library root: persists it to this Trove's own
    /// `config.toml` (`<home>/config.toml`, the same file every other client
    /// reads) and updates the in-memory config so it takes effect
    /// immediately for the rest of this process, without requiring a
    /// restart to pick up the file change.
    pub fn set_library_root(&mut self, root: &Path) -> Result<()> {
        crate::config::set_library_root(&self.home.join("config.toml"), root)?;
        self.config.library.root = Some(root.to_path_buf());
        Ok(())
    }

    /// One-time, local, offline backfill of `library_relative_path` for
    /// content archived before a library root existed or was declared (ADR
    /// 007, Group D2a). No re-read, no re-hash, no re-upload of any audio —
    /// purely a metadata pass over `source_path_original`, already recorded
    /// at commit time. Pushes the updated index once, only if anything was
    /// actually backfilled.
    pub fn backfill_slugs(&mut self, allow_offline: bool) -> Result<crate::library::BackfillSlugsReport> {
        let root = self.library_root().map(|p| p.to_path_buf()).ok_or_else(|| {
            crate::error::Error::config(
                "no library root declared — set one with `trove library root --set <path>` first",
            )
        })?;
        self.reconcile(allow_offline)?;
        let entries = self.archive.all()?;
        let (to_update, report) = crate::library::plan_slug_backfill(&root, &entries);
        for entry in &to_update {
            self.archive.upsert(entry)?;
        }
        if !to_update.is_empty() {
            self.push_index()?;
        }
        Ok(report)
    }

    /// Reconcile, then check every indexed entry actually has a
    /// correctly-sized object in the bucket. `deep` re-downloads and
    /// re-hashes every object instead of only checking presence/size — a
    /// full read of the archive, expensive, opt-in only (ADR 007, Group B2).
    pub fn archive_verify(
        &mut self,
        allow_offline: bool,
        deep: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<crate::archive::ArchiveVerifyReport> {
        self.reconcile(allow_offline)?;
        let entries = self.archive.all()?;
        crate::archive::verify_archive(self.store.as_ref(), &entries, deep, progress)
    }

    // --- Playlists -------------------------------------------------------

    pub fn playlist_create(&self, name: &str) -> Result<Playlist> {
        self.playlists.create(name)
    }

    pub fn playlist_list(&self) -> Result<Vec<Playlist>> {
        self.playlists.list()
    }

    pub fn playlist_get(&self, name: &str) -> Result<Playlist> {
        self.playlists.get(name)
    }

    pub fn playlist_add(&self, name: &str, track_ids: &[TrackId]) -> Result<()> {
        self.playlists.add_tracks(name, track_ids)
    }

    pub fn playlist_remove(&self, name: &str, track_id: &TrackId) -> Result<()> {
        self.playlists.remove_track(name, track_id)
    }

    // --- Sync / export ---------------------------------------------------

    fn resolve_playlist_entries(
        &mut self,
        name: &str,
        allow_offline: bool,
    ) -> Result<Vec<ArchiveEntry>> {
        self.reconcile(allow_offline)?;
        let playlist = self.playlists.get(name)?;
        let mut entries = Vec::new();
        for id in &playlist.track_ids {
            if let Some(entry) = self.archive.get(id)? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Resolve playlist entries and build a sync plan for a volume.
    pub fn plan_playlist_sync(
        &mut self,
        name: &str,
        mount: &Path,
        allow_offline: bool,
    ) -> Result<SyncPlan> {
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        let entries = self.resolve_playlist_entries(name, allow_offline)?;
        sync::plan_sync(
            &entries,
            self.config.export.layout,
            Some(&volume_db),
            Some(mount),
        )
    }

    /// Plan a sync from a query result set.
    pub fn plan_query_sync(
        &mut self,
        spec: &QuerySpec,
        mount: &Path,
        allow_offline: bool,
    ) -> Result<SyncPlan> {
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        let entries = self.query(spec, allow_offline)?;
        sync::plan_sync(
            &entries,
            self.config.export.layout,
            Some(&volume_db),
            Some(mount),
        )
    }

    /// Execute a playlist sync: transfer tracks and write the playlist file.
    pub fn run_playlist_sync(
        &mut self,
        name: &str,
        mount: &Path,
        allow_offline: bool,
    ) -> Result<(SyncPlan, usize, usize)> {
        let plan = self.plan_playlist_sync(name, mount, allow_offline)?;
        let (done, failed) = self.execute_sync_plan(name, mount, allow_offline, &plan)?;
        let entries = self.resolve_playlist_entries(name, allow_offline)?;
        let export = sync::export_playlist(
            name,
            &entries,
            self.config.export.layout,
            self.config.export.playlist_format,
            self.config.mixxx.relative_paths,
        );
        sync::write_playlist_export(mount, &export)?;
        Ok((plan, done, failed))
    }

    /// Continue the latest active sync job.
    pub fn resume_sync(&mut self, allow_offline: bool) -> Result<(usize, usize)> {
        let job = self
            .transfer_db
            .latest_active_job()?
            .ok_or_else(|| crate::error::Error::not_found("active sync job"))?;
        let mount = PathBuf::from(&job.mount_point);
        let entries = if let Some(name) = &job.playlist_name {
            self.resolve_playlist_entries(name, allow_offline)?
        } else {
            Vec::new()
        };
        let entries_by_id: std::collections::HashMap<String, ArchiveEntry> = entries
            .iter()
            .map(|e| (e.track_id.0.clone(), e.clone()))
            .collect();
        let identity = volume::read_identity(&mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        sync::run_sync_job(
            &mount,
            &entries_by_id,
            &self.transfer_db,
            &volume_db,
            self.store.as_ref(),
            &job.id,
        )
    }

    /// Verify expected tracks on a mounted volume.
    pub fn verify_volume_sync(
        &mut self,
        mount: &Path,
        playlist_name: Option<&str>,
        spec: Option<&QuerySpec>,
        allow_offline: bool,
    ) -> Result<(usize, usize, usize)> {
        let entries = if let Some(name) = playlist_name {
            self.resolve_playlist_entries(name, allow_offline)?
        } else if let Some(spec) = spec {
            self.query(spec, allow_offline)?
        } else {
            return Err(crate::error::Error::Other(
                "verify requires --playlist or query filters".into(),
            ));
        };
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        sync::verify_volume(mount, &entries, self.config.export.layout, &volume_db)
    }

    /// Export a logical playlist as a portable file body + relative paths.
    pub fn playlist_export(&mut self, name: &str, allow_offline: bool) -> Result<PlaylistExport> {
        let entries = self.resolve_playlist_entries(name, allow_offline)?;
        Ok(sync::export_playlist(
            name,
            &entries,
            self.config.export.layout,
            self.config.export.playlist_format,
            self.config.mixxx.relative_paths,
        ))
    }

    /// List host-side volume records.
    pub fn volume_list(&self) -> Result<Vec<crate::model::VolumeIdentity>> {
        if self.home == Path::new(":memory:") {
            return Ok(Vec::new());
        }
        volume::list_volumes(&self.home)
    }

    /// Volume identity plus file stats from the host DB.
    pub fn volume_status(
        &self,
        mount: &Path,
    ) -> Result<(crate::model::VolumeIdentity, VolumeFileStats)> {
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        Ok((identity, volume_db.stats()?))
    }

    /// Diff playlist or query tracks against a volume without transferring.
    pub fn volume_diff(
        &mut self,
        mount: &Path,
        playlist_name: Option<&str>,
        spec: Option<&QuerySpec>,
        allow_offline: bool,
    ) -> Result<Vec<VolumeDiffEntry>> {
        let entries = if let Some(name) = playlist_name {
            self.resolve_playlist_entries(name, allow_offline)?
        } else if let Some(spec) = spec {
            self.query(spec, allow_offline)?
        } else {
            return Err(crate::error::Error::Other(
                "diff requires --playlist or query filters".into(),
            ));
        };
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        sync::diff_volume(&entries, self.config.export.layout, &volume_db, mount)
    }

    fn execute_sync_plan(
        &mut self,
        playlist_name: &str,
        mount: &Path,
        allow_offline: bool,
        plan: &SyncPlan,
    ) -> Result<(usize, usize)> {
        let identity = volume::read_identity(mount)?
            .ok_or_else(|| crate::error::Error::not_found("volume identity on mount"))?;
        let volume_db = volume::open_volume_db(&self.home, &identity)?;
        let job_id = self.transfer_db.create_job(
            &identity.volume_id,
            &mount.display().to_string(),
            Some(playlist_name),
        )?;
        for transfer in &plan.transfers {
            self.transfer_db
                .insert_transfer(&job_id, &identity.volume_id, transfer)?;
        }
        let entries = self.resolve_playlist_entries(playlist_name, allow_offline)?;
        let entries_by_id: std::collections::HashMap<String, ArchiveEntry> = entries
            .iter()
            .map(|e| (e.track_id.0.clone(), e.clone()))
            .collect();
        sync::run_sync_job(
            mount,
            &entries_by_id,
            &self.transfer_db,
            &volume_db,
            self.store.as_ref(),
            &job_id,
        )
    }

    // --- Import ----------------------------------------------------------

    /// Plan a bulk import (scan + hash + dedupe + gather art), persist to
    /// `sync.sqlite`, and write a local manifest.
    ///
    /// Reconciles first (`allow_offline` controls the same offline-fallback
    /// behavior as [`Trove::reconcile`]). Without this, the archive-wide
    /// dedupe check inside `import::plan` only sees whatever this machine's
    /// local cache happened to already contain — cold on a fresh machine, or
    /// simply stale if another device committed matching content since this
    /// one last pulled — and would silently mint a duplicate `ArchiveEntry`
    /// for content the bucket already has (ADR 007, Group C2).
    pub fn import_plan(
        &mut self,
        source_root: &Path,
        options: &ImportOptions,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<ImportJob> {
        self.reconcile(allow_offline)?;
        let job = import::plan(
            source_root,
            import::DEFAULT_AUDIO_EXTENSIONS,
            options,
            Some(&self.archive),
            Some(&self.import_db),
            Some(&self.fingerprint_cache),
            self.library_root(),
            None,
            progress,
        )?;
        self.import_db.save_job(&job, options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Continue fingerprinting for a job interrupted during plan.
    ///
    /// Reconciles first, for the same reason as [`Trove::import_plan`] —
    /// newly discovered files in the resumed scan get the same archive-wide
    /// dedupe protection as a fresh plan would.
    pub fn import_continue_plan(
        &mut self,
        job_id: &str,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<ImportJob> {
        self.reconcile(allow_offline)?;
        let (job, options) = self.import_db.load_job(job_id)?;
        let job = import::plan(
            &job.source_root,
            import::DEFAULT_AUDIO_EXTENSIONS,
            &options,
            Some(&self.archive),
            Some(&self.import_db),
            Some(&self.fingerprint_cache),
            self.library_root(),
            Some(job_id),
            progress,
        )?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Upload + verify for a persisted job (no commit).
    pub fn import_run_job(
        &mut self,
        job_id: &str,
        progress: &mut dyn ImportProgress,
    ) -> Result<ImportJob> {
        let (mut job, options) = self.import_db.load_job(job_id)?;
        import::prepare_for_resume(&mut job);
        import::refresh_changed_files(&mut job)?;
        import::upload(
            &mut job,
            self.store.as_ref(),
            &self.paths,
            Some(&self.import_db),
            progress,
        )?;
        import::verify(
            &mut job,
            self.store.as_ref(),
            Some(&self.import_db),
            false,
            progress,
        )?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Re-verify staged objects for a persisted job. `deep` re-downloads and
    /// re-hashes each object instead of only checking size/presence — see
    /// `import::verify`'s doc comment for the cost/correctness trade-off.
    pub fn import_verify_job(
        &mut self,
        job_id: &str,
        deep: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<ImportJob> {
        let (mut job, options) = self.import_db.load_job(job_id)?;
        import::verify(
            &mut job,
            self.store.as_ref(),
            Some(&self.import_db),
            deep,
            progress,
        )?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Commit verified files, capture artwork, and push the canonical index.
    ///
    /// Reconciles first, same reason as [`Trove::import_plan`] but for the
    /// commit-time dedupe check (`import::commit`'s own `find_by_sha256`
    /// lookup) rather than the scan-time one — in the staged workflow
    /// (`plan` → `run` → `commit`), real time can pass between planning and
    /// committing, during which another machine may have pushed matching
    /// content. Reconciling only at plan time would leave this check exposed
    /// to exactly the same cold-cache gap C2 exists to close (ADR 007).
    pub fn import_commit_job(
        &mut self,
        job_id: &str,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<usize> {
        self.reconcile(allow_offline)?;
        let (mut job, options) = self.import_db.load_job(job_id)?;
        let committed = import::commit(
            &mut job,
            self.store.as_ref(),
            &self.paths,
            &self.archive,
            self.extractor.as_ref(),
            Some(&self.import_db),
            self.library_root(),
            progress,
        )?;
        for entry in &committed {
            self.archive.upsert(entry)?;
        }
        let artwork = import::capture_artwork(&mut job, self.store.as_ref(), &self.paths)?;
        self.append_artwork_manifest(&artwork)?;
        self.push_index()?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(committed.len())
    }

    /// Continue from the last safe state (fingerprint, upload, or verify).
    pub fn import_resume(
        &mut self,
        job_id: &str,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<ImportJob> {
        let (job, _) = self.import_db.load_job(job_id)?;
        if job.phase == Phase::Fingerprint {
            self.import_continue_plan(job_id, allow_offline, progress)?;
        }
        self.import_run_job(job_id, progress)
    }

    /// Structured per-job status.
    pub fn import_status(&self, job_id: &str) -> Result<ImportStatusReport> {
        self.import_db.status(job_id)
    }

    /// List import jobs (default: incomplete; pass `all` for full history).
    pub fn import_list(&self, all: bool) -> Result<Vec<ImportJobSummary>> {
        self.import_db.list_jobs(all)
    }

    /// Remove durable import bookkeeping for a job (local only).
    pub fn import_prune(&self, job_id: &str) -> Result<()> {
        self.import_db.prune_job(job_id)?;
        if self.home != Path::new(":memory:") {
            import::remove_manifest(&self.home, job_id)?;
        }
        Ok(())
    }

    /// Convenience one-shot: plan → upload → verify → commit.
    pub fn import_run_full(
        &mut self,
        source_root: &Path,
        options: &ImportOptions,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<(ImportJob, usize)> {
        let job = self.import_plan(source_root, options, allow_offline, progress)?;
        self.import_run_job(&job.id, progress)?;
        let committed = self.import_commit_job(&job.id, allow_offline, progress)?;
        let (job, _) = self.import_db.load_job(&job.id)?;
        Ok((job, committed))
    }

    /// Run a planned import to completion: upload → verify → commit, capture any
    /// co-located cover art, then advance the canonical index (only after commit).
    ///
    /// Reconciles first, same reasoning as [`Trove::import_commit_job`] — this
    /// method commits directly rather than going through it, so it needs its
    /// own reconcile call to get the same protection.
    pub fn import_run(
        &mut self,
        job: &mut ImportJob,
        allow_offline: bool,
        progress: &mut dyn ImportProgress,
    ) -> Result<usize> {
        self.reconcile(allow_offline)?;
        let options = self.loaded_options(&job.id).unwrap_or_default();
        self.import_db.save_job(job, &options)?;
        import::prepare_for_resume(job);
        import::refresh_changed_files(job)?;
        import::upload(
            job,
            self.store.as_ref(),
            &self.paths,
            Some(&self.import_db),
            progress,
        )?;
        import::verify(
            job,
            self.store.as_ref(),
            Some(&self.import_db),
            false,
            progress,
        )?;
        let committed = import::commit(
            job,
            self.store.as_ref(),
            &self.paths,
            &self.archive,
            self.extractor.as_ref(),
            Some(&self.import_db),
            self.library_root(),
            progress,
        )?;
        for entry in &committed {
            self.archive.upsert(entry)?;
        }
        let artwork = import::capture_artwork(job, self.store.as_ref(), &self.paths)?;
        self.append_artwork_manifest(&artwork)?;
        self.push_index()?;
        self.import_db.save_job(job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, job)?;
        }
        Ok(committed.len())
    }

    fn loaded_options(&self, job_id: &str) -> Result<ImportOptions> {
        let (_, options) = self.import_db.load_job(job_id)?;
        Ok(options)
    }

    /// Merge new artwork provenance records into the durable bucket manifest,
    /// deduping by (sha256, source_folder).
    fn append_artwork_manifest(&self, records: &[ArtworkRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let key = self.paths.artwork_manifest();
        let mut all = match self.store.get(&key) {
            Ok(bytes) => index::artwork_from_jsonl(&bytes)?,
            Err(crate::error::Error::NotFound(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let mut seen: std::collections::HashSet<(String, String)> = all
            .iter()
            .map(|r| (r.sha256.clone(), r.source_folder.clone()))
            .collect();
        for record in records {
            if seen.insert((record.sha256.clone(), record.source_folder.clone())) {
                all.push(record.clone());
            }
        }
        self.store.put(&key, &index::artwork_to_jsonl(&all)?)?;
        Ok(())
    }

    /// Push the local archive index up to the bucket as the new canonical
    /// generation.
    ///
    /// Writes an immutable, generation-keyed index object
    /// (`archive-index/<generation>.jsonl`, create-only) and only then
    /// CAS-advances the `schema-version.json` marker to point at it. Both
    /// writes are conditional, so two machines racing to push never produce a
    /// state where one machine's marker points at (or is decoupled from) the
    /// other's index content — see ADR 007, Group B1, for the exact failure
    /// mode this closes relative to a naive "CAS the marker only" design.
    ///
    /// Retries from a fresh read on either conflict (someone else claimed the
    /// generation number, or advanced the marker between our read and our
    /// write) up to a small retry cap — a push under real contention makes
    /// progress because every retry sees the winner's already-durable write,
    /// never the same stale state twice.
    pub fn push_index(&mut self) -> Result<u64> {
        const MAX_RETRIES: u32 = 10;

        for _attempt in 0..MAX_RETRIES {
            let (remote_generation, marker_etag) = self.head_marker()?;

            // If the bucket is ahead of what this local cache last knew about
            // — because this is a retry after losing a race, or simply
            // because another machine pushed since we last reconciled — merge
            // that newer content in *additively* before computing what to
            // push. A destructive reconcile (`replace_all`, which does
            // `DELETE FROM tracks` then reinserts) would silently drop any
            // entries this instance has committed locally but not yet pushed
            // itself; upsert-only picks up the other writer's entries without
            // losing ours. This is what actually prevents a lost update on
            // retry — CAS alone only stops two writers from silently sharing
            // one generation's key, it does not by itself keep a retrying
            // writer's stale local view from omitting what it lost the race
            // on (see ADR 007, Group B1).
            if let Some(remote_gen) = remote_generation {
                if self.archive.generation()? != Some(remote_gen) {
                    self.merge_remote_generation(remote_gen)?;
                }
            }

            let next_generation = remote_generation.unwrap_or(0) + 1;
            let entries = self.archive.all()?;
            let jsonl = index::entries_to_jsonl(&entries)?;
            let index_key = self.paths.archive_index_generation(next_generation);

            // Step 1: claim this generation's index object. Create-only —
            // the key has never been used before, so a conflict here means
            // another writer already claimed `next_generation`.
            match self.store.put_if_match(&index_key, None, &jsonl)? {
                PutOutcome::Written(_) => {}
                PutOutcome::Conflict { .. } => continue,
            }

            // Step 2: only now advance the marker to point at the index we
            // just durably wrote. If this loses the race (someone else's
            // marker write landed between our read and this write), our
            // already-written index object at `index_key` is simply
            // orphaned — harmless, since readers only trust the marker.
            let marker = SchemaVersion {
                schema_version: CURRENT_SCHEMA_VERSION,
                generation: next_generation,
                updated_at: Utc::now(),
            };
            match self.store.put_if_match(
                &self.paths.schema_version(),
                marker_etag.as_deref(),
                &index::schema_version_bytes(&marker)?,
            )? {
                PutOutcome::Written(_) => {
                    self.archive.set_generation(next_generation)?;
                    return Ok(next_generation);
                }
                PutOutcome::Conflict { .. } => continue,
            }
        }

        Err(crate::error::Error::store(format!(
            "push_index: exceeded {MAX_RETRIES} retries under contention"
        )))
    }

    /// Current remote generation (if the marker exists yet) and its etag, as
    /// the compare-and-swap baseline for `push_index`. `head()` alone gives
    /// the etag but not the parsed generation number, so a small `get()`
    /// follows when the marker exists — the marker is a few dozen bytes of
    /// JSON, so this costs nothing meaningful, and any drift between the two
    /// calls is harmless: the final `put_if_match` uses the etag captured
    /// here, so any real change is still caught there regardless.
    fn head_marker(&self) -> Result<(Option<u64>, Option<String>)> {
        let marker_key = self.paths.schema_version();
        match self.store.head(&marker_key)? {
            Some(meta) => {
                let bytes = self.store.get(&marker_key)?;
                let marker = index::schema_version_from_bytes(&bytes)?;
                Ok((Some(marker.generation), meta.etag))
            }
            None => Ok((None, None)),
        }
    }

    /// Pull the given generation's index and upsert its entries into the
    /// local cache — additive, never deletes. Used by `push_index` to absorb
    /// another writer's already-committed entries before retrying, without
    /// destroying this instance's own not-yet-pushed local entries the way a
    /// full `reconcile()` (which deletes and replaces) would.
    fn merge_remote_generation(&mut self, generation: u64) -> Result<()> {
        let jsonl = self.store.get(&self.paths.archive_index_generation(generation))?;
        let entries = index::entries_from_jsonl(&jsonl)?;
        for entry in &entries {
            self.archive.upsert(entry)?;
        }
        Ok(())
    }
}
