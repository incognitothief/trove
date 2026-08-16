//! Archive-wide integrity check: does every indexed entry actually have a
//! corresponding, correctly-sized (and, in `deep` mode, correctly-hashed)
//! object in the bucket? (ADR 007, Group B2.)
//!
//! Deliberately separate from `reconcile`: reconcile trusts the index is
//! correct and pulls it; this checks whether the index's claims about the
//! bucket are actually true.

use serde::Serialize;

use crate::error::Result;
use crate::model::{ArchiveEntry, TrackId};
use crate::store::ObjectStore;

/// Result of an `archive verify` pass.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ArchiveVerifyReport {
    pub total: usize,
    pub verified: usize,
    /// Indexed but no object exists at `object_key` in the bucket.
    pub missing: Vec<TrackId>,
    /// Object exists but its size doesn't match the indexed `size_bytes`.
    pub size_mismatch: Vec<TrackId>,
    /// Only populated in `deep` mode: object exists, size matches, but a
    /// re-hash of its bytes doesn't match the indexed `sha256`.
    pub hash_mismatch: Vec<TrackId>,
    /// Whether `deep` (re-download + re-hash) was used for this pass.
    pub deep: bool,
}

impl ArchiveVerifyReport {
    /// Whether every indexed entry checked out.
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.size_mismatch.is_empty() && self.hash_mismatch.is_empty()
    }
}

/// Verify every entry in `entries` against `store`. `deep` re-downloads and
/// re-hashes each object instead of only checking presence/size — expensive
/// (a full read of every object in the archive), so it's opt-in, never the
/// default for a large archive.
pub fn verify_archive(
    store: &dyn ObjectStore,
    entries: &[ArchiveEntry],
    deep: bool,
) -> Result<ArchiveVerifyReport> {
    let mut report = ArchiveVerifyReport {
        total: entries.len(),
        deep,
        ..Default::default()
    };

    for entry in entries {
        match store.head(&entry.object_key)? {
            None => report.missing.push(entry.track_id.clone()),
            Some(meta) if meta.size_bytes != entry.size_bytes => {
                report.size_mismatch.push(entry.track_id.clone())
            }
            Some(_) if deep => {
                let bytes = store.get(&entry.object_key)?;
                let actual_sha = crate::import::hash_bytes(&bytes);
                if actual_sha == entry.sha256 {
                    report.verified += 1;
                } else {
                    report.hash_mismatch.push(entry.track_id.clone());
                }
            }
            Some(_) => report.verified += 1,
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::stub::StubStore;
    use chrono::Utc;

    fn entry(id: &str, key: &str, sha: &str, size: u64) -> ArchiveEntry {
        let now = Utc::now();
        ArchiveEntry {
            track_id: TrackId::from(id),
            object_key: key.to_string(),
            size_bytes: size,
            sha256: sha.to_string(),
            metadata: Default::default(),
            tags: Vec::new(),
            imported_at: now,
            updated_at: now,
            source_path_original: None,
            artwork_object_key: None,
        }
    }

    #[test]
    fn shallow_verify_catches_missing_and_size_mismatch_but_not_corruption() {
        let store = StubStore::new();
        store.put("music/a.mp3", b"hello world").unwrap();
        // "b" is indexed but was never actually written to the bucket.
        // "a" is present but the index disagrees about its size.
        let entries = vec![
            entry("a", "music/a.mp3", &crate::import::hash_bytes(b"hello world"), 999),
            entry("b", "music/b.mp3", "deadbeef", 5),
        ];

        let report = verify_archive(&store, &entries, false).unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.missing, vec![TrackId::from("b")]);
        assert_eq!(report.size_mismatch, vec![TrackId::from("a")]);
        assert!(report.hash_mismatch.is_empty(), "shallow mode never checks hashes");
    }

    #[test]
    fn deep_verify_catches_content_corruption_shallow_mode_misses() {
        let store = StubStore::new();
        // Right size, right key, but the bytes don't match the indexed hash
        // (simulating bit-rot or a corrupted upload that still landed at the
        // expected size).
        store.put("music/c.mp3", b"corrupted!!").unwrap();
        let entries = vec![entry(
            "c",
            "music/c.mp3",
            &crate::import::hash_bytes(b"original!!!"),
            11,
        )];

        let shallow = verify_archive(&store, &entries, false).unwrap();
        assert!(
            shallow.is_clean(),
            "shallow mode can't see content corruption — size matches"
        );

        let deep = verify_archive(&store, &entries, true).unwrap();
        assert!(!deep.is_clean());
        assert_eq!(deep.hash_mismatch, vec![TrackId::from("c")]);
    }

    #[test]
    fn deep_verify_passes_genuinely_intact_content() {
        let store = StubStore::new();
        store.put("music/d.mp3", b"intact bytes").unwrap();
        let entries = vec![entry(
            "d",
            "music/d.mp3",
            &crate::import::hash_bytes(b"intact bytes"),
            12,
        )];

        let report = verify_archive(&store, &entries, true).unwrap();
        assert!(report.is_clean());
        assert_eq!(report.verified, 1);
    }
}
