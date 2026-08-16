//! One-time, local, offline backfill of `library_relative_path` for content
//! archived before D2 existed, or before a library root was declared (ADR
//! 007, Group D2a).
//!
//! `source_path_original` has always stored the file's full absolute path at
//! commit time (`import/mod.rs`'s `commit()` — `file.path`, not anything
//! relative to a job's own `source_root`), so computing a slug retroactively
//! is a cheap, local, metadata-only operation once a root is declared: no
//! re-read, no re-hash, no re-upload of any audio. This exists specifically
//! because an already-backfilled archive (the common case, not a
//! hypothetical) shouldn't have to choose between staying on the old path
//! forever or re-importing everything to get the new one.

use std::path::Path;

use serde::Serialize;

use crate::model::ArchiveEntry;

/// Result of a `library backfill-slugs` pass.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackfillSlugsReport {
    pub total: usize,
    /// Newly given a slug this pass.
    pub backfilled: usize,
    /// Already had one (an earlier pass, or committed after a root was
    /// already declared) — left untouched.
    pub already_had_slug: usize,
    /// `source_path_original` doesn't fall under the declared root — stays
    /// `None`, same as any file that was never part of "the library"
    /// concept (e.g. an ad hoc import predating a consistent layout).
    pub outside_root: usize,
    /// No `source_path_original` recorded at all to compute a slug from.
    pub no_source_path: usize,
}

/// Pure computation: given the current entries and a declared library root,
/// decide which entries need a slug backfilled and what it should be.
/// Touches no database or bucket — `Trove::backfill_slugs` applies the
/// result. Kept separate specifically so this is unit-testable without a
/// real `ArchiveDb`/store, the same shape `archive::verify_archive` already
/// uses.
pub fn plan_slug_backfill(
    library_root: &Path,
    entries: &[ArchiveEntry],
) -> (Vec<ArchiveEntry>, BackfillSlugsReport) {
    let mut to_update = Vec::new();
    let mut report = BackfillSlugsReport {
        total: entries.len(),
        ..Default::default()
    };

    for entry in entries {
        if entry.library_relative_path.is_some() {
            report.already_had_slug += 1;
            continue;
        }
        let Some(source) = entry.source_path_original.as_deref() else {
            report.no_source_path += 1;
            continue;
        };
        match super::compute_slug(library_root, Path::new(source)) {
            Some(slug) => {
                let mut updated = entry.clone();
                updated.library_relative_path = Some(slug);
                report.backfilled += 1;
                to_update.push(updated);
            }
            None => report.outside_root += 1,
        }
    }

    (to_update, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackId;
    use chrono::Utc;

    fn entry(id: &str, source_path: Option<&str>, slug: Option<&str>) -> ArchiveEntry {
        let now = Utc::now();
        ArchiveEntry {
            track_id: TrackId::from(id),
            object_key: format!("music/{id}.mp3"),
            size_bytes: 100,
            sha256: format!("sha-{id}"),
            metadata: Default::default(),
            tags: Vec::new(),
            imported_at: now,
            updated_at: now,
            source_path_original: source_path.map(str::to_string),
            artwork_object_key: None,
            library_relative_path: slug.map(str::to_string),
        }
    }

    #[test]
    fn backfills_entries_whose_source_path_falls_under_the_root() {
        let root = Path::new("/Volumes/T7/music/library");
        let entries = vec![entry(
            "a",
            Some("/Volumes/T7/music/library/Artist/Track.mp3"),
            None,
        )];

        let (updated, report) = plan_slug_backfill(root, &entries);
        assert_eq!(report.total, 1);
        assert_eq!(report.backfilled, 1);
        assert_eq!(updated.len(), 1);
        assert_eq!(
            updated[0].library_relative_path.as_deref(),
            Some("Artist/Track.mp3")
        );
    }

    #[test]
    fn leaves_entries_that_already_have_a_slug_untouched() {
        let root = Path::new("/Volumes/T7/music/library");
        let entries = vec![entry(
            "a",
            Some("/Volumes/T7/music/library/Artist/Track.mp3"),
            Some("Artist/Track.mp3"),
        )];

        let (updated, report) = plan_slug_backfill(root, &entries);
        assert_eq!(report.already_had_slug, 1);
        assert_eq!(report.backfilled, 0);
        assert!(updated.is_empty());
    }

    #[test]
    fn leaves_entries_outside_the_root_and_without_a_source_path_alone() {
        let root = Path::new("/Volumes/T7/music/library");
        let entries = vec![
            entry("a", Some("/Volumes/Other/random.mp3"), None),
            entry("b", None, None),
        ];

        let (updated, report) = plan_slug_backfill(root, &entries);
        assert_eq!(report.outside_root, 1);
        assert_eq!(report.no_source_path, 1);
        assert_eq!(report.backfilled, 0);
        assert!(updated.is_empty());
    }
}
