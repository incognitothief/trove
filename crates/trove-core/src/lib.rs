//! # trove-core
//!
//! The single authoritative implementation of Trove's domain logic. Every other
//! surface — `trove-cli`, `trove-serverd`, and the React UI it serves — is a
//! thin client over this crate and holds no business logic of its own.
//!
//! See `docs/adr/000-bootstrap.md` for the architecture this implements.
//!
//! ## Layout
//!
//! - [`config`] — local `~/.trove/config.toml`.
//! - [`model`] — domain types (tracks, entries, playlists, volumes).
//! - [`store`] — the durable object store (bucket) abstraction.
//! - [`db`] — disposable local SQLite caches/bookkeeping.
//! - [`archive`] — the canonical bucket index and reconciliation.
//! - [`query`] — declarative search over the archive.
//! - [`playlist`] — logical crates/playlists.
//! - [`volume`] — removable performance volumes.
//! - [`sync`] — resumable transfer/export planning.
//! - [`import`] — resumable, crash-safe bulk import.
//! - [`facade`] — the [`Trove`] entry point clients drive.

pub mod archive;
pub mod config;
pub mod db;
pub mod error;
pub mod facade;
pub mod import;
pub mod metadata;
pub mod model;
pub mod playlist;
pub mod query;
pub mod store;
pub mod sync;
pub mod volume;

pub use config::Config;
pub use db::import::{ImportJobSummary, ImportStatusReport};
pub use error::{Error, Result};
pub use facade::Trove;
pub use import::ImportOptions;
pub use import::{ImportProgress, ImportProgressEvent, ImportProgressKind, NoopImportProgress};
pub use model::{ArchiveEntry, Metadata, Playlist, TrackId};
pub use query::QuerySpec;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::index::{self, BucketPaths};
    use crate::archive::reconcile::ReconcileReport;
    use crate::import::{FileState, NoopImportProgress, Phase};
    use crate::model::SchemaVersion;
    use crate::store::{stub::StubStore, ObjectStore};
    use chrono::Utc;

    fn test_config() -> Config {
        Config::from_toml(
            r#"
            [bucket]
            name = "test-bucket"
            region = "us-east-1"
            "#,
        )
        .unwrap()
    }

    fn sample_entry(id: &str, artist: &str, bpm: f32) -> ArchiveEntry {
        let now = Utc::now();
        ArchiveEntry {
            track_id: TrackId::from(id),
            object_key: format!("music/{id}.flac"),
            size_bytes: 1024,
            sha256: "deadbeef".into(),
            metadata: Metadata {
                artist: Some(artist.into()),
                bpm: Some(bpm),
                ..Default::default()
            },
            tags: vec!["house".into()],
            imported_at: now,
            updated_at: now,
            source_path_original: None,
            artwork_object_key: None,
        }
    }

    /// Build a stub bucket store pre-seeded with a JSONL index + generation.
    fn seeded_store(entries: &[ArchiveEntry], generation: u64) -> Box<dyn ObjectStore> {
        let paths = BucketPaths::new(".trove", "music");
        let store = StubStore::new();
        let jsonl = index::entries_to_jsonl(entries).unwrap();
        store.put(&paths.archive_index_jsonl(), &jsonl).unwrap();
        let marker = SchemaVersion {
            schema_version: crate::archive::CURRENT_SCHEMA_VERSION,
            generation,
            updated_at: Utc::now(),
        };
        store
            .put(
                &paths.schema_version(),
                &index::schema_version_bytes(&marker).unwrap(),
            )
            .unwrap();
        Box::new(store)
    }

    #[test]
    fn reconcile_hydrates_then_no_ops() {
        let entries = vec![
            sample_entry("a", "Theo Parrish", 120.0),
            sample_entry("b", "Rick Wade", 122.0),
        ];
        let store = seeded_store(&entries, 1);
        let mut trove = Trove::in_memory_with_store(test_config(), store).unwrap();

        match trove.reconcile(false).unwrap() {
            ReconcileReport::Rehydrated { entries_loaded, .. } => assert_eq!(entries_loaded, 2),
            other => panic!("expected rehydrate, got {other:?}"),
        }
        assert!(matches!(
            trove.reconcile(false).unwrap(),
            ReconcileReport::UpToDate { .. }
        ));
    }

    #[test]
    fn empty_bucket_reconciles_cleanly() {
        let mut trove = Trove::in_memory(test_config()).unwrap();
        assert_eq!(
            trove.reconcile(false).unwrap(),
            ReconcileReport::EmptyBucket
        );
    }

    #[test]
    fn query_filters_by_artist_and_bpm() {
        let entries = vec![
            sample_entry("a", "Theo Parrish", 120.0),
            sample_entry("b", "Rick Wade", 122.0),
        ];
        let store = seeded_store(&entries, 1);
        let mut trove = Trove::in_memory_with_store(test_config(), store).unwrap();

        let results = trove
            .query(&QuerySpec::new().artist("Theo Parrish"), false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].track_id, TrackId::from("a"));

        let by_bpm = trove
            .query(&QuerySpec::new().bpm_range(Some(121.0), None), false)
            .unwrap();
        assert_eq!(by_bpm.len(), 1);
        assert_eq!(by_bpm[0].track_id, TrackId::from("b"));
    }

    #[test]
    fn import_skips_hidden_and_captures_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Artist/Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 - track.mp3"), b"audio-bytes").unwrap();
        std::fs::write(album.join("cover.jpg"), b"JPEGDATA").unwrap();
        std::fs::write(album.join(".hidden.mp3"), b"nope").unwrap();
        std::fs::write(album.join("._track.mp3"), b"appledouble").unwrap();
        std::fs::write(album.join("notes.txt"), b"not audio, not image").unwrap();

        // Default options: exclude dotfiles, capture artwork.
        let mut progress = NoopImportProgress;
        let job = crate::import::plan(
            dir.path(),
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(job.files.len(), 1, "only the real audio track is imported");
        assert_eq!(job.artwork.len(), 1, "the co-located cover is captured");

        // Capturing writes a content-addressed object + a provenance record.
        let store = StubStore::new();
        let paths = BucketPaths::new(".trove", "music");
        let mut job = job;
        let records = crate::import::capture_artwork(&mut job, &store, &paths).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].object_key.starts_with("artwork/"));
        assert!(store.exists(&records[0].object_key).unwrap());
        assert_eq!(records[0].source_folder, album.display().to_string());

        // Opting out of artwork yields no candidates.
        let mut progress = NoopImportProgress;
        let no_art = crate::import::plan(
            dir.path(),
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions {
                capture_artwork: false,
                ..Default::default()
            },
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert!(no_art.artwork.is_empty());
    }

    #[test]
    fn import_plan_accepts_single_audio_file() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("01 Moth Love.aiff");
        std::fs::write(&track, b"audio-bytes").unwrap();

        let mut progress = NoopImportProgress;
        let job = crate::import::plan(
            &track,
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(job.files.len(), 1);
        assert_eq!(job.source_root, track);
        assert_eq!(job.files[0].path, track);
    }

    #[test]
    fn import_single_file_skips_co_located_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Album");
        std::fs::create_dir_all(&album).unwrap();
        let track = album.join("01 Moth Love.aiff");
        std::fs::write(&track, b"audio-bytes").unwrap();
        std::fs::write(album.join("cover.jpg"), b"JPEGDATA").unwrap();

        let mut progress = NoopImportProgress;
        let job = crate::import::plan(
            &track,
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(job.files.len(), 1);
        assert!(
            job.artwork.is_empty(),
            "single-file import should not gather folder artwork"
        );

        let with_art = crate::import::plan(
            &track,
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions {
                artwork_paths: vec![album.join("cover.jpg")],
                ..Default::default()
            },
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(with_art.artwork.len(), 1);
        assert_eq!(with_art.artwork[0].path, album.join("cover.jpg"));

        let folder_job = crate::import::plan(
            &album,
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(folder_job.artwork.len(), 1);
    }

    #[test]
    fn import_commits_content_addressed_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01 - track.mp3"), b"audio-bytes").unwrap();

        let mut trove = Trove::in_memory(test_config()).unwrap();
        let mut progress = NoopImportProgress;
        let mut job = trove
            .import_plan(dir.path(), &ImportOptions::default(), &mut progress)
            .unwrap();
        let sha = job.files[0].sha256.clone();
        let committed = trove.import_run(&mut job, &mut progress).unwrap();
        assert_eq!(committed, 1);

        let results = trove.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object_key, format!("music/{sha}.mp3"));
        assert_eq!(results[0].sha256, sha);
    }

    #[test]
    fn import_skips_archive_duplicate_sha() {
        let dir1 = tempfile::tempdir().unwrap();
        std::fs::write(dir1.path().join("first.mp3"), b"same-bytes").unwrap();

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("second.mp3"), b"same-bytes").unwrap();

        let mut trove = Trove::in_memory(test_config()).unwrap();
        let mut progress = NoopImportProgress;
        let mut first = trove
            .import_plan(dir1.path(), &ImportOptions::default(), &mut progress)
            .unwrap();
        assert_eq!(trove.import_run(&mut first, &mut progress).unwrap(), 1);

        let mut second = trove
            .import_plan(dir2.path(), &ImportOptions::default(), &mut progress)
            .unwrap();
        assert_eq!(second.files.len(), 1);
        assert_eq!(second.files[0].state, FileState::Duplicate);
        assert_eq!(trove.import_run(&mut second, &mut progress).unwrap(), 0);
        assert_eq!(trove.query(&QuerySpec::new(), false).unwrap().len(), 1);
    }

    #[test]
    fn import_durable_resume_skips_uploaded_files() {
        use std::sync::{Arc, Mutex};

        struct SharedStub(Arc<Mutex<StubStore>>);

        impl ObjectStore for SharedStub {
            fn get(&self, key: &str) -> Result<Vec<u8>> {
                self.0.lock().unwrap().get(key)
            }
            fn put(&self, key: &str, bytes: &[u8]) -> Result<crate::store::ObjectMeta> {
                self.0.lock().unwrap().put(key, bytes)
            }
            fn exists(&self, key: &str) -> Result<bool> {
                self.0.lock().unwrap().exists(key)
            }
            fn head(&self, key: &str) -> Result<Option<crate::store::ObjectMeta>> {
                self.0.lock().unwrap().head(key)
            }
            fn copy(&self, from: &str, to: &str) -> Result<crate::store::ObjectMeta> {
                self.0.lock().unwrap().copy(from, to)
            }
            fn list(&self, prefix: &str) -> Result<Vec<String>> {
                self.0.lock().unwrap().list(prefix)
            }
        }

        struct FailAlwaysOnB {
            inner: Arc<Mutex<StubStore>>,
        }

        impl ObjectStore for FailAlwaysOnB {
            fn get(&self, key: &str) -> Result<Vec<u8>> {
                self.inner.lock().unwrap().get(key)
            }
            fn put(&self, key: &str, bytes: &[u8]) -> Result<crate::store::ObjectMeta> {
                if key.contains("b.mp3") {
                    return Err(Error::store("simulated persistent upload failure"));
                }
                self.inner.lock().unwrap().put(key, bytes)
            }
            fn exists(&self, key: &str) -> Result<bool> {
                self.inner.lock().unwrap().exists(key)
            }
            fn head(&self, key: &str) -> Result<Option<crate::store::ObjectMeta>> {
                self.inner.lock().unwrap().head(key)
            }
            fn copy(&self, from: &str, to: &str) -> Result<crate::store::ObjectMeta> {
                self.inner.lock().unwrap().copy(from, to)
            }
            fn list(&self, prefix: &str) -> Result<Vec<String>> {
                self.inner.lock().unwrap().list(prefix)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mp3"), b"audio-a").unwrap();
        std::fs::write(dir.path().join("b.mp3"), b"audio-b").unwrap();
        let home = tempfile::tempdir().unwrap();

        let shared = Arc::new(Mutex::new(StubStore::new()));
        let config = test_config();
        let mut trove = Trove::open_with_store(
            config.clone(),
            home.path(),
            Box::new(FailAlwaysOnB {
                inner: shared.clone(),
            }),
        )
        .unwrap();

        let mut progress = NoopImportProgress;
        let job = trove
            .import_plan(dir.path(), &ImportOptions::default(), &mut progress)
            .unwrap();
        assert_eq!(job.files.len(), 2);

        let partial = trove.import_run_job(&job.id, &mut progress).unwrap();
        assert_eq!(partial.files[0].state, FileState::Verified);
        assert_eq!(partial.files[1].state, FileState::Failed);

        let mut trove2 =
            Trove::open_with_store(config, home.path(), Box::new(SharedStub(shared))).unwrap();
        let mut progress2 = NoopImportProgress;
        let resumed = trove2.import_resume(&job.id, &mut progress2).unwrap();
        assert_eq!(resumed.files[0].state, FileState::Verified);
        assert_eq!(resumed.files[1].state, FileState::Verified);

        let committed = trove2.import_commit_job(&job.id, &mut progress2).unwrap();
        assert_eq!(committed, 2);
        assert_eq!(trove2.import_status(&job.id).unwrap().stats.committed, 2);
    }

    #[test]
    fn import_resume_completes_interrupted_fingerprint() {
        use sha2::{Digest, Sha256};

        fn sha256_hex(bytes: &[u8]) -> String {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            format!("{:x}", hasher.finalize())
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mp3"), b"audio-a").unwrap();
        std::fs::write(dir.path().join("b.mp3"), b"audio-b").unwrap();
        std::fs::write(dir.path().join("c.mp3"), b"audio-c").unwrap();
        let home = tempfile::tempdir().unwrap();

        let db = crate::db::import::ImportDb::open(&home.path().join("sync.sqlite")).unwrap();
        let id = "partial-fingerprint-job";
        let staging = format!("staging/{id}");
        db.create_job_shell(id, dir.path(), &staging, &ImportOptions::default())
            .unwrap();
        let paths = vec![
            dir.path().join("a.mp3"),
            dir.path().join("b.mp3"),
            dir.path().join("c.mp3"),
        ];
        db.set_total_files(id, paths.len()).unwrap();
        db.insert_pending_files(id, &paths).unwrap();

        for (path, bytes) in [
            (paths[0].clone(), b"audio-a".as_slice()),
            (paths[1].clone(), b"audio-b".as_slice()),
        ] {
            let meta = std::fs::metadata(&path).unwrap();
            db.update_file(
                id,
                &crate::import::PlannedFile {
                    path,
                    size: meta.len(),
                    mtime: meta.modified().ok().map(|t| {
                        use std::time::UNIX_EPOCH;
                        let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
                        format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
                    }),
                    sha256: sha256_hex(bytes),
                    state: FileState::Hashed,
                    object_key: None,
                    track_id: None,
                    etag: None,
                    error: None,
                    attempts: 0,
                },
            )
            .unwrap();
        }

        let trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new())).unwrap();
        let mut progress = NoopImportProgress;
        let jobs = trove.import_list(false).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].phase, Phase::Fingerprint);

        let job = trove.import_continue_plan(id, &mut progress).unwrap();
        assert_eq!(job.phase, Phase::Dedupe);
        assert_eq!(job.files.len(), 3);
        assert!(job
            .files
            .iter()
            .all(|f| { matches!(f.state, FileState::Hashed | FileState::Duplicate) }));
    }

    #[test]
    fn playlist_roundtrip() {
        let trove = Trove::in_memory(test_config()).unwrap();
        trove.playlist_create("tonight").unwrap();
        trove
            .playlist_add("tonight", &[TrackId::from("a"), TrackId::from("b")])
            .unwrap();
        let pl = trove.playlist_get("tonight").unwrap();
        assert_eq!(pl.track_ids.len(), 2);
        trove
            .playlist_remove("tonight", &TrackId::from("a"))
            .unwrap();
        assert_eq!(trove.playlist_get("tonight").unwrap().track_ids.len(), 1);
    }

    #[test]
    fn import_prune_drops_durable_job() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let db = crate::db::import::ImportDb::open(&home.path().join("sync.sqlite")).unwrap();
        let id = "ghost-job";
        db.create_job_shell(
            id,
            dir.path(),
            "staging/ghost-job",
            &ImportOptions::default(),
        )
        .unwrap();
        assert!(db.load_job(id).is_ok());

        db.prune_job(id).unwrap();
        assert!(db.load_job(id).is_err());
        assert!(db.list_jobs(true).unwrap().is_empty());
    }
}
