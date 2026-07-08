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
pub use error::{Error, Result};
pub use facade::Trove;
pub use model::{ArchiveEntry, Metadata, Playlist, TrackId};
pub use query::QuerySpec;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::index::{self, BucketPaths};
    use crate::archive::reconcile::ReconcileReport;
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
}
