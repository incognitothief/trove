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
use crate::db::import::{ImportDb, ImportJobSummary, ImportStatusReport};
use crate::error::Result;
use crate::import::{self, ImportJob, ImportOptions};
use crate::metadata::{MetadataExtractor, StubExtractor};
use crate::model::SchemaVersion;
use crate::model::{ArchiveEntry, ArtworkRecord, Playlist, TrackId};
use crate::playlist::PlaylistDb;
use crate::query::QuerySpec;
use crate::store::{stub::StubStore, ObjectStore};
use crate::sync::{self, SyncPlan};

/// Aggregates configuration, local databases, and the durable object store.
pub struct Trove {
    pub config: Config,
    pub home: PathBuf,
    pub paths: BucketPaths,
    archive: ArchiveDb,
    playlists: PlaylistDb,
    import_db: ImportDb,
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
        Ok(Trove {
            config,
            home: home.to_path_buf(),
            paths,
            archive,
            playlists,
            import_db,
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

    /// Resolve the entries in a playlist and build a sync plan for a volume.
    pub fn plan_playlist_sync(&mut self, name: &str, allow_offline: bool) -> Result<SyncPlan> {
        self.reconcile(allow_offline)?;
        let playlist = self.playlists.get(name)?;
        let mut entries = Vec::new();
        for id in &playlist.track_ids {
            if let Some(entry) = self.archive.get(id)? {
                entries.push(entry);
            }
        }
        Ok(sync::plan_playlist_sync(
            &entries,
            self.config.export.layout,
        ))
    }

    // --- Import ----------------------------------------------------------

    /// Plan a bulk import (scan + hash + dedupe + gather art), persist to
    /// `sync.sqlite`, and write a local manifest.
    pub fn import_plan(&self, source_root: &Path, options: &ImportOptions) -> Result<ImportJob> {
        let job = import::plan(
            source_root,
            import::DEFAULT_AUDIO_EXTENSIONS,
            options,
            Some(&self.archive),
        )?;
        self.import_db.save_job(&job, options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Upload + verify for a persisted job (no commit).
    pub fn import_run_job(&mut self, job_id: &str) -> Result<ImportJob> {
        let (mut job, options) = self.import_db.load_job(job_id)?;
        import::prepare_for_resume(&mut job);
        import::refresh_changed_files(&mut job)?;
        import::upload(
            &mut job,
            self.store.as_ref(),
            &self.paths,
            Some(&self.import_db),
        )?;
        import::verify(&mut job, self.store.as_ref(), Some(&self.import_db))?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Re-verify staged objects for a persisted job.
    pub fn import_verify_job(&mut self, job_id: &str) -> Result<ImportJob> {
        let (mut job, options) = self.import_db.load_job(job_id)?;
        import::verify(&mut job, self.store.as_ref(), Some(&self.import_db))?;
        self.import_db.save_job(&job, &options)?;
        if self.home != Path::new(":memory:") {
            import::write_manifest(&self.home, &job)?;
        }
        Ok(job)
    }

    /// Commit verified files, capture artwork, and push the canonical index.
    pub fn import_commit_job(&mut self, job_id: &str) -> Result<usize> {
        let (mut job, options) = self.import_db.load_job(job_id)?;
        let committed = import::commit(
            &mut job,
            self.store.as_ref(),
            &self.paths,
            &self.archive,
            self.extractor.as_ref(),
            Some(&self.import_db),
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

    /// Continue upload + verify from the last safe state (no commit).
    pub fn import_resume(&mut self, job_id: &str) -> Result<ImportJob> {
        self.import_run_job(job_id)
    }

    /// Structured per-job status.
    pub fn import_status(&self, job_id: &str) -> Result<ImportStatusReport> {
        self.import_db.status(job_id)
    }

    /// List import jobs (default: incomplete; pass `all` for full history).
    pub fn import_list(&self, all: bool) -> Result<Vec<ImportJobSummary>> {
        self.import_db.list_jobs(all)
    }

    /// Convenience one-shot: plan → upload → verify → commit.
    pub fn import_run_full(
        &mut self,
        source_root: &Path,
        options: &ImportOptions,
    ) -> Result<(ImportJob, usize)> {
        let job = self.import_plan(source_root, options)?;
        self.import_run_job(&job.id)?;
        let committed = self.import_commit_job(&job.id)?;
        let (job, _) = self.import_db.load_job(&job.id)?;
        Ok((job, committed))
    }

    /// Run a planned import to completion: upload → verify → commit, capture any
    /// co-located cover art, then advance the canonical index (only after commit).
    pub fn import_run(&mut self, job: &mut ImportJob) -> Result<usize> {
        let options = self.loaded_options(&job.id).unwrap_or_default();
        self.import_db.save_job(job, &options)?;
        import::prepare_for_resume(job);
        import::refresh_changed_files(job)?;
        import::upload(job, self.store.as_ref(), &self.paths, Some(&self.import_db))?;
        import::verify(job, self.store.as_ref(), Some(&self.import_db))?;
        let committed = import::commit(
            job,
            self.store.as_ref(),
            &self.paths,
            &self.archive,
            self.extractor.as_ref(),
            Some(&self.import_db),
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
    /// generation (writes `archive-index.jsonl` + `schema-version.json`).
    pub fn push_index(&mut self) -> Result<u64> {
        let entries = self.archive.all()?;
        let jsonl = index::entries_to_jsonl(&entries)?;
        self.store.put(&self.paths.archive_index_jsonl(), &jsonl)?;

        let next_generation = self.remote_generation()?.unwrap_or(0) + 1;
        let marker = SchemaVersion {
            schema_version: CURRENT_SCHEMA_VERSION,
            generation: next_generation,
            updated_at: Utc::now(),
        };
        self.store.put(
            &self.paths.schema_version(),
            &index::schema_version_bytes(&marker)?,
        )?;
        self.archive.set_generation(next_generation)?;
        Ok(next_generation)
    }

    fn remote_generation(&self) -> Result<Option<u64>> {
        match self.store.get(&self.paths.schema_version()) {
            Ok(bytes) => Ok(Some(index::schema_version_from_bytes(&bytes)?.generation)),
            Err(crate::error::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
