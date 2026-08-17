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
//! - [`library`] — the declared library root and what's anchored to it:
//!   cross-drive identity, the shape scan, the backfill Plan.
//! - [`facade`] — the [`Trove`] entry point clients drive.

pub mod archive;
pub mod config;
pub mod db;
pub mod error;
pub mod facade;
pub mod import;
pub mod library;
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
            library_relative_path: None,
        }
    }

    /// Build a stub bucket store pre-seeded with a JSONL index + generation.
    fn seeded_store(entries: &[ArchiveEntry], generation: u64) -> Box<dyn ObjectStore> {
        let paths = BucketPaths::new(".trove", "music");
        let store = StubStore::new();
        let jsonl = index::entries_to_jsonl(entries).unwrap();
        store
            .put(&paths.archive_index_generation(generation), &jsonl)
            .unwrap();
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
            None,
            None,
            &mut progress,
        )
        .unwrap();
        assert_eq!(folder_job.artwork.len(), 1);
    }

    #[test]
    fn import_plan_populates_fingerprint_cache_keyed_by_path_size_mtime() {
        use crate::db::fingerprint::FingerprintCache;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"original bytes").unwrap();
        let cache = FingerprintCache::in_memory().unwrap();
        let mut progress = NoopImportProgress;

        let job = crate::import::plan(
            dir.path(),
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            Some(&cache),
            None,
            None,
            &mut progress,
        )
        .unwrap();
        let expected_sha = crate::import::hash_bytes(b"original bytes");
        assert_eq!(job.files[0].sha256, expected_sha);

        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()));
        assert_eq!(
            cache.lookup(&path, meta.len(), mtime.as_deref()).unwrap(),
            Some(expected_sha),
            "plan() must record the fingerprint it just computed, keyed by \
             exactly the (path, size, mtime) a later lookup would use"
        );
    }

    #[test]
    fn import_plan_trusts_a_cached_fingerprint_over_the_files_actual_bytes() {
        use crate::db::fingerprint::FingerprintCache;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"whatever bytes are on disk").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()));

        // Poison the cache with a hash that does *not* match the file's real
        // bytes, at exactly the (path, size, mtime) key a real plan() call
        // would look up. This directly proves plan() trusts a cache hit
        // instead of independently re-hashing — a much stronger and more
        // deterministic check than trying to race file corruption against
        // mtime, which real filesystems don't let a test control precisely.
        let cache = FingerprintCache::in_memory().unwrap();
        cache
            .upsert(&path, meta.len(), mtime.as_deref(), "poisoned-hash-proves-cache-was-trusted")
            .unwrap();

        let mut progress = NoopImportProgress;
        let job = crate::import::plan(
            dir.path(),
            crate::import::DEFAULT_AUDIO_EXTENSIONS,
            &ImportOptions::default(),
            None,
            None,
            Some(&cache),
            None,
            None,
            &mut progress,
        )
        .unwrap();

        assert_eq!(
            job.files[0].sha256, "poisoned-hash-proves-cache-was-trusted",
            "a fresh plan() against a path matching a cached (size, mtime) must \
             reuse the cached fingerprint rather than reading and re-hashing \
             the file itself"
        );
    }

    #[test]
    fn import_commits_content_addressed_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01 - track.mp3"), b"audio-bytes").unwrap();

        let mut trove = Trove::in_memory(test_config()).unwrap();
        let mut progress = NoopImportProgress;
        let mut job = trove
            .import_plan(dir.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        let sha = job.files[0].sha256.clone();
        let committed = trove.import_run(&mut job, false, &mut progress).unwrap();
        assert_eq!(committed, 1);

        let results = trove.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object_key, format!("music/{sha}.mp3"));
        assert_eq!(results[0].sha256, sha);
    }

    #[test]
    fn commit_records_library_relative_path_only_when_a_root_is_declared() {
        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Theo Parrish")).unwrap();
        std::fs::write(
            library.path().join("Theo Parrish/Track.mp3"),
            b"audio-bytes",
        )
        .unwrap();

        // No library root declared: stays None, same as before this unit.
        let mut trove_no_root = Trove::in_memory(test_config()).unwrap();
        let mut progress = NoopImportProgress;
        let (_, committed) = trove_no_root
            .import_run_full(library.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(committed, 1);
        let results = trove_no_root.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(results[0].library_relative_path, None);

        // Library root declared: the slug is computed relative to it, not
        // to whatever path this particular `import` call used.
        let home = tempfile::tempdir().unwrap();
        let mut trove_with_root =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        trove_with_root.set_library_root(library.path()).unwrap();
        let (_, committed) = trove_with_root
            .import_run_full(library.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(committed, 1);
        let results = trove_with_root.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(
            results[0].library_relative_path.as_deref(),
            Some("Theo Parrish/Track.mp3")
        );
    }

    #[test]
    fn slug_and_size_fast_path_trusts_the_archive_over_the_files_actual_bytes() {
        use crate::import::DuplicateReason;

        // "Machine A" layout: real content, real hash.
        let library_a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library_a.path().join("Artist")).unwrap();
        std::fs::write(
            library_a.path().join("Artist/Track.mp3"),
            b"original content",
        )
        .unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        trove.set_library_root(library_a.path()).unwrap();
        let mut progress = NoopImportProgress;
        let (_, committed) = trove
            .import_run_full(library_a.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(committed, 1);
        let original_sha = trove.query(&QuerySpec::new(), false).unwrap()[0].sha256.clone();

        // "Machine B" (or a replacement drive): same relative structure and
        // size at *Artist/Track.mp3*, but deliberately different bytes --
        // proves the fast path trusts the archive match rather than
        // silently re-reading and re-hashing, the same way the
        // fingerprint-cache poison test proves C1's trust.
        let library_b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library_b.path().join("Artist")).unwrap();
        assert_eq!(b"original content".len(), b"corrupted!!!!!!!".len());
        std::fs::write(
            library_b.path().join("Artist/Track.mp3"),
            b"corrupted!!!!!!!",
        )
        .unwrap();

        trove.set_library_root(library_b.path()).unwrap();
        let job = trove
            .import_plan(library_b.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();

        assert_eq!(job.files.len(), 1);
        assert_eq!(job.files[0].state, FileState::Duplicate);
        assert_eq!(
            job.files[0].duplicate_reason,
            Some(DuplicateReason::SlugAndSize)
        );
        assert_eq!(
            job.files[0].sha256, original_sha,
            "must trust the archive's recorded hash, not re-hash library_b's actual (corrupted) bytes"
        );
    }

    #[test]
    fn slug_match_with_different_size_falls_through_to_a_real_hash() {
        use crate::import::DuplicateReason;

        let library_a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library_a.path().join("Artist")).unwrap();
        std::fs::write(library_a.path().join("Artist/Track.mp3"), b"short").unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        trove.set_library_root(library_a.path()).unwrap();
        let mut progress = NoopImportProgress;
        trove
            .import_run_full(library_a.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();

        // Same relative path, genuinely different (and different-length)
        // content -- a real re-rip or upgrade at the same catalog position,
        // not a duplicate. The coarse filter must not make this call itself.
        let library_b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library_b.path().join("Artist")).unwrap();
        std::fs::write(
            library_b.path().join("Artist/Track.mp3"),
            b"a much longer replacement file",
        )
        .unwrap();

        trove.set_library_root(library_b.path()).unwrap();
        let job = trove
            .import_plan(library_b.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();

        assert_eq!(job.files.len(), 1);
        assert_ne!(
            job.files[0].duplicate_reason,
            Some(DuplicateReason::SlugAndSize),
            "a size mismatch must not be trusted as a slug+size match"
        );
        assert_eq!(
            job.files[0].sha256,
            crate::import::hash_bytes(b"a much longer replacement file"),
            "must fall through to a real hash of library_b's actual content"
        );
    }

    #[test]
    fn creates_and_pushes_a_backfill_plan_then_lists_it_back() {
        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist A")).unwrap();
        std::fs::create_dir_all(library.path().join("Artist B")).unwrap();
        std::fs::write(library.path().join("Artist A/Track 1.mp3"), b"12345").unwrap();
        std::fs::write(library.path().join("Artist B/Track 2.mp3"), b"123").unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();

        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_eq!(plan.library_root, library.path());
        assert_eq!(plan.chunks.len(), 2);

        // Re-listing re-reads from the store rather than trusting the
        // in-memory value just returned, proving it actually landed in the
        // bucket and round-trips through JSON correctly.
        let listed = trove.list_backfill_plans(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].plan_id, plan.plan_id);
        assert_eq!(listed[0], plan);

        let filtered_out = trove
            .list_backfill_plans(Some(std::path::Path::new("/somewhere/else")))
            .unwrap();
        assert!(filtered_out.is_empty());

        let filtered_in = trove.list_backfill_plans(Some(library.path())).unwrap();
        assert_eq!(filtered_in.len(), 1);

        // Never rewritten: a second plan for the same root is a distinct
        // plan_id, not an update to the first.
        let plan2 = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_ne!(plan.plan_id, plan2.plan_id);
        assert_eq!(trove.list_backfill_plans(None).unwrap().len(), 2);
    }

    #[test]
    fn records_chunk_events_and_folds_them_into_status_via_a_real_bucket_round_trip() {
        use crate::library::{ChunkEventKind, ChunkState};
        use std::sync::{Arc, Mutex};

        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist A")).unwrap();
        std::fs::create_dir_all(library.path().join("Artist B")).unwrap();
        std::fs::write(library.path().join("Artist A/Track1.mp3"), b"12345").unwrap();
        std::fs::write(library.path().join("Artist B/Track2.mp3"), b"123").unwrap();

        let shared = Arc::new(Mutex::new(StubStore::new()));
        let config = test_config();
        let home = tempfile::tempdir().unwrap();
        let mut trove = Trove::open_with_store(
            config.clone(),
            home.path(),
            Box::new(SharedStub(shared.clone())),
        )
        .unwrap();
        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_eq!(plan.chunks.len(), 2);
        let chunk_0 = plan.chunks[0].chunk_id.clone();
        let chunk_1 = plan.chunks[1].chunk_id.clone();

        // Nothing touched yet -- every chunk is untouched.
        let status = trove.chunk_status(&plan.plan_id).unwrap();
        assert!(status.iter().all(|s| s.state == ChunkState::Untouched));

        trove
            .record_chunk_event(&plan.plan_id, &chunk_0, ChunkEventKind::Claimed, "machine-a")
            .unwrap();
        let status = trove.chunk_status(&plan.plan_id).unwrap();
        let chunk_0_status = status.iter().find(|s| s.chunk_id == chunk_0).unwrap();
        assert_eq!(chunk_0_status.state, ChunkState::Claimed);
        let chunk_1_status = status.iter().find(|s| s.chunk_id == chunk_1).unwrap();
        assert_eq!(chunk_1_status.state, ChunkState::Untouched);

        trove
            .record_chunk_event(
                &plan.plan_id,
                &chunk_0,
                ChunkEventKind::Completed,
                "machine-a",
            )
            .unwrap();
        let status = trove.chunk_status(&plan.plan_id).unwrap();
        let chunk_0_status = status.iter().find(|s| s.chunk_id == chunk_0).unwrap();
        assert_eq!(chunk_0_status.state, ChunkState::Completed);
        assert_eq!(chunk_0_status.claims.len(), 1);
        assert_eq!(chunk_0_status.completions.len(), 1);

        // A second, entirely independent Trove handle sharing the same
        // underlying bucket (simulating a second machine) reconstructs the
        // exact same status purely by pulling the plan and folding its
        // event log -- no local state shared between the two handles at
        // all, only the bucket.
        let home2 = tempfile::tempdir().unwrap();
        let trove2 = Trove::open_with_store(
            config,
            home2.path(),
            Box::new(SharedStub(shared)),
        )
        .unwrap();
        let status2 = trove2.chunk_status(&plan.plan_id).unwrap();
        assert_eq!(status2, status);
    }

    #[test]
    fn plan_and_event_round_trip_survives_a_real_filesystem_backed_bucket() {
        // Every other Plan/event test uses StubStore's flat in-memory
        // HashMap, which can't catch a bug specific to a real hierarchical
        // store: the event key layout (`backfill-plans/<id>/events/<id>.json`)
        // needs FsStore to actually create nested directories on `put`, and
        // `list` to recurse into them. This is the same store a real "local"
        // bucket (region = "local") uses.
        use crate::library::{ChunkEventKind, ChunkState};
        use crate::store::fs::FsStore;

        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist")).unwrap();
        std::fs::write(library.path().join("Artist/Track.mp3"), b"12345").unwrap();

        let bucket_dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut trove = Trove::open_with_store(
            test_config(),
            home.path(),
            Box::new(FsStore::new(bucket_dir.path().to_path_buf())),
        )
        .unwrap();

        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_eq!(plan.chunks.len(), 1);
        let chunk_id = plan.chunks[0].chunk_id.clone();

        trove
            .record_chunk_event(&plan.plan_id, &chunk_id, ChunkEventKind::Claimed, "laptop")
            .unwrap();
        trove
            .record_chunk_event(&plan.plan_id, &chunk_id, ChunkEventKind::Completed, "laptop")
            .unwrap();

        // Actually landed as real, separate files under the expected
        // nested path on disk, not just readable back through the trait.
        let events_dir = bucket_dir
            .path()
            .join(".trove/backfill-plans")
            .join(&plan.plan_id)
            .join("events");
        let files: Vec<_> = std::fs::read_dir(&events_dir).unwrap().collect();
        assert_eq!(files.len(), 2, "each event is its own file under {events_dir:?}");

        // A fresh Trove re-opened against the same on-disk bucket (no
        // shared in-process state at all) reconstructs status purely by
        // reading files back off disk.
        let home2 = tempfile::tempdir().unwrap();
        let trove2 = Trove::open_with_store(
            test_config(),
            home2.path(),
            Box::new(FsStore::new(bucket_dir.path().to_path_buf())),
        )
        .unwrap();
        let status = trove2.chunk_status(&plan.plan_id).unwrap();
        assert_eq!(status[0].state, ChunkState::Completed);

        // Listing plans still only sees the plan document itself, not the
        // event files nested underneath it -- the exact bug this unit's
        // BucketPaths/list_backfill_plans change guards against.
        let plans = trove2.list_backfill_plans(None).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].plan_id, plan.plan_id);
    }

    #[test]
    fn claim_picks_the_next_untouched_chunk_and_actually_imports_it() {
        use crate::library::ChunkState;

        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist A")).unwrap();
        std::fs::create_dir_all(library.path().join("Artist B")).unwrap();
        std::fs::write(library.path().join("Artist A/Track1.mp3"), b"12345").unwrap();
        std::fs::write(library.path().join("Artist B/Track2.mp3"), b"123").unwrap();
        std::fs::write(library.path().join("Loose.mp3"), b"loose").unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_eq!(plan.chunks.len(), 2);
        let chunk_0 = plan.chunks[0].chunk_id.clone(); // Artist A + loose root files
        let chunk_1 = plan.chunks[1].chunk_id.clone(); // Artist B

        let mut progress = NoopImportProgress;

        // No explicit chunk id -- pick-next must land on the first
        // untouched chunk in plan order, which also covers the loose file.
        let report = trove
            .claim_backfill_chunk(
                &plan.plan_id,
                None,
                false,
                "machine-a",
                false,
                &ImportOptions::default(),
                &mut progress,
            )
            .unwrap();
        assert_eq!(report.chunk_id, chunk_0);
        assert_eq!(report.targets.len(), 2, "Artist A folder + Loose.mp3");
        assert_eq!(report.tracks_committed, 2);

        let status = trove.chunk_status(&plan.plan_id).unwrap();
        let s0 = status.iter().find(|s| s.chunk_id == chunk_0).unwrap();
        assert_eq!(s0.state, ChunkState::Completed);
        let s1 = status.iter().find(|s| s.chunk_id == chunk_1).unwrap();
        assert_eq!(s1.state, ChunkState::Untouched);

        // The actual archive now has both tracks from chunk 0's targets.
        let entries = trove.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(entries.len(), 2);

        // Claiming again with no explicit id must move on to chunk 1, not
        // redo chunk 0 -- chunk 0 is no longer "untouched".
        let report2 = trove
            .claim_backfill_chunk(
                &plan.plan_id,
                None,
                false,
                "machine-a",
                false,
                &ImportOptions::default(),
                &mut progress,
            )
            .unwrap();
        assert_eq!(report2.chunk_id, chunk_1);

        // Now every chunk is completed -- a third claim with no explicit id
        // must refuse rather than silently redoing work.
        let err = trove
            .claim_backfill_chunk(
                &plan.plan_id,
                None,
                false,
                "machine-a",
                false,
                &ImportOptions::default(),
                &mut progress,
            )
            .unwrap_err();
        assert!(err.to_string().contains("no untouched chunks remain"));
    }

    #[test]
    fn claim_with_an_unknown_explicit_chunk_id_refuses_with_a_clear_error() {
        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist")).unwrap();
        std::fs::write(library.path().join("Artist/Track.mp3"), b"12345").unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        let mut progress = NoopImportProgress;

        let err = trove
            .claim_backfill_chunk(
                &plan.plan_id,
                Some("does-not-exist"),
                false,
                "machine-a",
                false,
                &ImportOptions::default(),
                &mut progress,
            )
            .unwrap_err();
        assert!(err.to_string().contains("does-not-exist"));

        // Refusing to pick a chunk must not have left any stray claim event
        // behind for a chunk that was never actually chosen.
        let status = trove.chunk_status(&plan.plan_id).unwrap();
        assert!(status
            .iter()
            .all(|s| s.state == crate::library::ChunkState::Untouched));
    }

    #[test]
    fn claim_flags_a_stat_mismatch_when_a_folder_changed_after_the_plan_was_made_but_still_completes(
    ) {
        // The one real risk case Group E4 exists for: a folder name collides
        // with genuinely different content underneath by the time it's
        // actually claimed. This must surface as a hint, not block the claim.
        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Artist")).unwrap();
        std::fs::write(library.path().join("Artist/Track1.mp3"), b"12345").unwrap();

        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        let plan = trove
            .create_backfill_plan(library.path(), 1, &crate::library::ShapeOptions::default())
            .unwrap();
        assert_eq!(plan.chunks[0].estimated_audio_file_count, 1);

        // Between plan creation and claim, this folder gained a lot more
        // content than the shape scan originally saw.
        for i in 0..10 {
            std::fs::write(
                library.path().join(format!("Artist/Extra{i}.mp3")),
                b"unexpected extra content that was not there when planned",
            )
            .unwrap();
        }

        let mut progress = NoopImportProgress;
        let report = trove
            .claim_backfill_chunk(
                &plan.plan_id,
                None,
                false,
                "machine-a",
                false,
                &ImportOptions::default(),
                &mut progress,
            )
            .unwrap();

        assert!(report.stat_check.mismatch, "11 actual files vs. 1 estimated must flag");
        assert_eq!(report.stat_check.estimated_audio_file_count, 1);
        assert_eq!(report.stat_check.actual_audio_file_count, 11);

        // A mismatch is a hint, not a correctness boundary -- the claim
        // must still have completed normally.
        let status = trove.chunk_status(&plan.plan_id).unwrap();
        assert_eq!(status[0].state, crate::library::ChunkState::Completed);
    }

    #[test]
    fn backfill_slugs_gives_already_archived_content_a_slug_without_touching_bytes() {
        let library = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library.path().join("Theo Parrish")).unwrap();
        std::fs::write(
            library.path().join("Theo Parrish/Track.mp3"),
            b"audio-bytes",
        )
        .unwrap();

        // Import *without* a declared root first — simulates content
        // archived before this feature existed, exactly the real-world case
        // this unit is for.
        let home = tempfile::tempdir().unwrap();
        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new()))
                .unwrap();
        let mut progress = NoopImportProgress;
        let (_, committed) = trove
            .import_run_full(library.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(committed, 1);
        let before = trove.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(before[0].library_relative_path, None);

        // No root declared yet: backfill refuses rather than guessing one.
        assert!(trove.backfill_slugs(false).is_err());

        // Declare the root retroactively, then backfill.
        trove.set_library_root(library.path()).unwrap();
        let report = trove.backfill_slugs(false).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.backfilled, 1);
        assert_eq!(report.already_had_slug, 0);

        let after = trove.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(
            after[0].library_relative_path.as_deref(),
            Some("Theo Parrish/Track.mp3")
        );
        // Content identity is untouched -- same sha256, same object key.
        assert_eq!(after[0].sha256, before[0].sha256);
        assert_eq!(after[0].object_key, before[0].object_key);

        // Running it again is a clean no-op (nothing left to backfill).
        let second_report = trove.backfill_slugs(false).unwrap();
        assert_eq!(second_report.backfilled, 0);
        assert_eq!(second_report.already_had_slug, 1);
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
            .import_plan(dir1.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(trove.import_run(&mut first, false, &mut progress).unwrap(), 1);

        let mut second = trove
            .import_plan(dir2.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(second.files.len(), 1);
        assert_eq!(second.files[0].state, FileState::Duplicate);
        assert_eq!(trove.import_run(&mut second, false, &mut progress).unwrap(), 0);
        assert_eq!(trove.query(&QuerySpec::new(), false).unwrap().len(), 1);
    }

    #[test]
    fn import_durable_resume_skips_uploaded_files() {
        use std::sync::{Arc, Mutex};

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
            fn put_if_match(
                &self,
                key: &str,
                expected_etag: Option<&str>,
                bytes: &[u8],
            ) -> Result<crate::store::PutOutcome> {
                self.inner
                    .lock()
                    .unwrap()
                    .put_if_match(key, expected_etag, bytes)
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
            .import_plan(dir.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(job.files.len(), 2);

        let partial = trove.import_run_job(&job.id, &mut progress).unwrap();
        assert_eq!(partial.files[0].state, FileState::Verified);
        assert_eq!(partial.files[1].state, FileState::Failed);

        let mut trove2 =
            Trove::open_with_store(config, home.path(), Box::new(SharedStub(shared))).unwrap();
        let mut progress2 = NoopImportProgress;
        let resumed = trove2
            .import_resume(&job.id, false, &mut progress2)
            .unwrap();
        assert_eq!(resumed.files[0].state, FileState::Verified);
        assert_eq!(resumed.files[1].state, FileState::Verified);

        let committed = trove2
            .import_commit_job(&job.id, false, &mut progress2)
            .unwrap();
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
                    duplicate_reason: None,
                },
            )
            .unwrap();
        }

        let mut trove =
            Trove::open_with_store(test_config(), home.path(), Box::new(StubStore::new())).unwrap();
        let mut progress = NoopImportProgress;
        let jobs = trove.import_list(false).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].phase, Phase::Fingerprint);

        let job = trove.import_continue_plan(id, false, &mut progress).unwrap();
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

    /// Test-only `ObjectStore` sharing one `StubStore` across multiple
    /// independent `Trove` instances (simulating separate machines pointed at
    /// the same bucket). `StubStore`'s internal mutex makes this genuinely
    /// atomic, unlike `FsStore` — see ADR 007, Group B1, on why that
    /// distinction matters for what these tests can actually prove.
    struct SharedStub(std::sync::Arc<std::sync::Mutex<StubStore>>);

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
        fn put_if_match(
            &self,
            key: &str,
            expected_etag: Option<&str>,
            bytes: &[u8],
        ) -> Result<crate::store::PutOutcome> {
            self.0.lock().unwrap().put_if_match(key, expected_etag, bytes)
        }
    }

    #[test]
    fn import_plan_reconciles_first_and_recognizes_already_archived_content() {
        use std::sync::{Arc, Mutex};

        let shared = Arc::new(Mutex::new(StubStore::new()));
        let config = test_config();
        let mut progress = NoopImportProgress;

        // "Machine A": imports and pushes a track.
        let dir_a = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("original.mp3"), b"identical-bytes").unwrap();
        let home_a = tempfile::tempdir().unwrap();
        let mut trove_a = Trove::open_with_store(
            config.clone(),
            home_a.path(),
            Box::new(SharedStub(shared.clone())),
        )
        .unwrap();
        let mut job_a = trove_a
            .import_plan(dir_a.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(trove_a.import_run(&mut job_a, false, &mut progress).unwrap(), 1);

        // "Machine B": a brand-new local cache (never reconciled, never seen
        // this bucket before — the exact cold-cache condition this unit
        // exists to close) imports the *same bytes* from a different path.
        // Before this fix, `import_plan`'s dedupe check would query B's
        // empty local cache, miss, and mark the file `Hashed` instead of
        // `Duplicate` — going on to mint a second `ArchiveEntry` for content
        // already in the bucket, even though `store.exists` would correctly
        // avoid re-uploading the actual bytes.
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_b.path().join("different_name.mp3"), b"identical-bytes").unwrap();
        let home_b = tempfile::tempdir().unwrap();
        let mut trove_b = Trove::open_with_store(
            config.clone(),
            home_b.path(),
            Box::new(SharedStub(shared.clone())),
        )
        .unwrap();
        let job_b = trove_b
            .import_plan(dir_b.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(
            job_b.files[0].state,
            FileState::Duplicate,
            "reconcile-before-plan should let B recognize A's already-archived \
             content instead of treating it as new"
        );

        let mut job_b = job_b;
        let committed = trove_b.import_run(&mut job_b, false, &mut progress).unwrap();
        assert_eq!(
            committed, 0,
            "a recognized duplicate must not be committed as a second entry"
        );

        // A fresh, third machine must see exactly one archive entry for this
        // content, not two.
        let home_c = tempfile::tempdir().unwrap();
        let mut trove_c =
            Trove::open_with_store(config, home_c.path(), Box::new(SharedStub(shared))).unwrap();
        let results = trove_c.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(
            results.len(),
            1,
            "cold-cache import of already-archived content must not create a duplicate entry"
        );
    }

    #[test]
    fn push_index_merges_entries_committed_by_another_writer() {
        use std::sync::{Arc, Mutex};

        let shared = Arc::new(Mutex::new(StubStore::new()));
        let config = test_config();
        let mut progress = NoopImportProgress;

        // "Machine A": a fresh local cache, imports one track and pushes.
        let dir_a = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("a.mp3"), b"audio-a").unwrap();
        let home_a = tempfile::tempdir().unwrap();
        let mut trove_a = Trove::open_with_store(
            config.clone(),
            home_a.path(),
            Box::new(SharedStub(shared.clone())),
        )
        .unwrap();
        let mut job_a = trove_a
            .import_plan(dir_a.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(trove_a.import_run(&mut job_a, false, &mut progress).unwrap(), 1);

        // "Machine B": a *completely separate* local cache that has never
        // reconciled and knows nothing about machine A's push. Imports a
        // different track and pushes — this is the exact scenario the review
        // caught: does B's push silently drop A's already-canonical entry?
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_b.path().join("b.mp3"), b"audio-b").unwrap();
        let home_b = tempfile::tempdir().unwrap();
        let mut trove_b = Trove::open_with_store(
            config.clone(),
            home_b.path(),
            Box::new(SharedStub(shared.clone())),
        )
        .unwrap();
        let mut job_b = trove_b
            .import_plan(dir_b.path(), &ImportOptions::default(), false, &mut progress)
            .unwrap();
        assert_eq!(trove_b.import_run(&mut job_b, false, &mut progress).unwrap(), 1);

        // A third, fresh machine reconciling from scratch must see both
        // tracks. Before the fix, B's push would have written its own
        // `archive.all()` (missing A's entry) straight to the fixed
        // `archive-index.jsonl` key, silently overwriting A's contribution.
        let home_c = tempfile::tempdir().unwrap();
        let mut trove_c =
            Trove::open_with_store(config, home_c.path(), Box::new(SharedStub(shared))).unwrap();
        let results = trove_c.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(
            results.len(),
            2,
            "machine B's push must not drop machine A's already-committed entry"
        );
    }

    #[test]
    fn push_index_no_lost_update_under_real_concurrent_pushes() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        const WRITERS: usize = 6;

        let shared = Arc::new(Mutex::new(StubStore::new()));
        let config = test_config();

        let handles: Vec<_> = (0..WRITERS)
            .map(|i| {
                let shared = shared.clone();
                let config = config.clone();
                thread::spawn(move || {
                    let dir = tempfile::tempdir().unwrap();
                    std::fs::write(
                        dir.path().join(format!("track-{i}.mp3")),
                        format!("audio-{i}").as_bytes(),
                    )
                    .unwrap();
                    let home = tempfile::tempdir().unwrap();
                    let mut trove = Trove::open_with_store(
                        config,
                        home.path(),
                        Box::new(SharedStub(shared)),
                    )
                    .unwrap();
                    let mut progress = NoopImportProgress;
                    let mut job = trove
                        .import_plan(dir.path(), &ImportOptions::default(), false, &mut progress)
                        .unwrap();
                    trove.import_run(&mut job, false, &mut progress)
                })
            })
            .collect();

        for h in handles {
            assert_eq!(
                h.join().unwrap().unwrap(),
                1,
                "every concurrent writer's commit must eventually succeed under contention, \
                 never be silently lost or hard-fail"
            );
        }

        let home_check = tempfile::tempdir().unwrap();
        let mut checker =
            Trove::open_with_store(config, home_check.path(), Box::new(SharedStub(shared)))
                .unwrap();
        let results = checker.query(&QuerySpec::new(), false).unwrap();
        assert_eq!(
            results.len(),
            WRITERS,
            "no writer's commit should be lost under real concurrent pushes \
             (found {} of {WRITERS} expected tracks)",
            results.len()
        );
    }
}
