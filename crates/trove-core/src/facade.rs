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
use crate::error::Result;
use crate::import::{self, ImportJob};
use crate::metadata::{MetadataExtractor, StubExtractor};
use crate::model::SchemaVersion;
use crate::model::{ArchiveEntry, Playlist, TrackId};
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
        Ok(Trove {
            config,
            home: home.to_path_buf(),
            paths,
            archive,
            playlists,
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

    /// Plan a bulk import (scan + hash + dedupe). Dry-run friendly.
    pub fn import_plan(&self, source_root: &Path) -> Result<ImportJob> {
        import::plan(source_root, import::DEFAULT_AUDIO_EXTENSIONS)
    }

    /// Run a planned import to completion: upload → verify → commit, advancing
    /// the local archive index only after commit.
    pub fn import_run(&mut self, job: &mut ImportJob) -> Result<usize> {
        import::upload(job, self.store.as_ref(), &self.paths)?;
        import::verify(job, self.store.as_ref())?;
        let committed = import::commit(
            job,
            self.store.as_ref(),
            &self.paths,
            self.extractor.as_ref(),
        )?;
        for entry in &committed {
            self.archive.upsert(entry)?;
        }
        // The index only advances after commit; push the new canonical index.
        self.push_index()?;
        Ok(committed.len())
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
