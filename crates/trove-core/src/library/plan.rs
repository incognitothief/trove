//! The Backfill Plan — durable, bucket-pushed, generated from a shape scan
//! (ADR 007, Group E2).
//!
//! A Plan describes *scope*: which folders under a declared library root
//! get grouped into which chunks for a coordinated, possibly multi-machine
//! backfill. It is written once and never rewritten — progress is tracked
//! separately, as an append-only event log keyed under the plan (Group E3),
//! not by mutating this document. That's what lets this layer skip
//! compare-and-swap entirely: a Plan is a pure create, and a stale read of
//! it is never possible since it never changes after creation.

use serde::{Deserialize, Serialize};

use crate::library::shape::LibraryShape;

/// One chunk of a Backfill Plan: the folder(s) it covers and the rough
/// stats the shape scan observed for them, used later as a cheap sanity
/// check (Group E4) rather than a correctness boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanChunk {
    /// Stable ordinal within this plan ("0", "1", ...), not globally unique
    /// — always addressed together with the plan's id.
    pub chunk_id: String,
    /// Relative-path slugs (D2 format) of the immediate root subdirectories
    /// this chunk covers, in the shape scan's alphabetical order.
    pub folders: Vec<String>,
    /// Whether this chunk also covers the library root's loose files (the
    /// ones sitting directly in the root, outside any subdirectory). There's
    /// no folder to chunk those by, so rather than inventing a synthetic
    /// chunk for them, they're folded into the first chunk of the plan —
    /// this is only ever `true` on `chunks[0]`, and only when the shape scan
    /// found any.
    pub includes_root_files: bool,
    pub estimated_audio_file_count: u64,
    pub estimated_audio_bytes: u64,
    pub estimated_total_file_count: u64,
    pub estimated_total_bytes: u64,
}

/// A durable, immutable Backfill Plan document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackfillPlan {
    pub plan_id: String,
    pub library_root: std::path::PathBuf,
    /// RFC 3339 timestamp of when this plan was generated.
    pub created_at: String,
    /// The folder-count lever used to build this plan: how many immediate
    /// subdirectories were grouped into each chunk.
    pub chunk_folders: u32,
    pub chunks: Vec<PlanChunk>,
}

/// Build a Plan from an already-completed shape scan. Pure and
/// deterministic given its inputs — the facade is responsible for minting
/// `plan_id`/`created_at` and pushing the result to the bucket.
///
/// `chunk_folders` groups that many consecutive subdirectories (in the
/// shape scan's alphabetical order) into each chunk; must be at least 1.
pub fn build_plan(
    plan_id: String,
    created_at: String,
    shape: &LibraryShape,
    chunk_folders: u32,
) -> BackfillPlan {
    let chunk_folders = chunk_folders.max(1);
    let mut chunks = Vec::new();

    for (index, group) in shape.subtrees.chunks(chunk_folders as usize).enumerate() {
        let mut chunk = PlanChunk {
            chunk_id: index.to_string(),
            folders: Vec::new(),
            includes_root_files: false,
            estimated_audio_file_count: 0,
            estimated_audio_bytes: 0,
            estimated_total_file_count: 0,
            estimated_total_bytes: 0,
        };
        for subtree in group {
            if let Some(relative_path) = &subtree.relative_path {
                chunk.folders.push(relative_path.clone());
            }
            chunk.estimated_audio_file_count += subtree.audio_file_count;
            chunk.estimated_audio_bytes += subtree.audio_bytes;
            chunk.estimated_total_file_count += subtree.total_file_count;
            chunk.estimated_total_bytes += subtree.total_bytes;
        }
        chunks.push(chunk);
    }

    if let Some(root_files) = &shape.root_files {
        match chunks.first_mut() {
            Some(first) => {
                first.includes_root_files = true;
                first.estimated_audio_file_count += root_files.audio_file_count;
                first.estimated_audio_bytes += root_files.audio_bytes;
                first.estimated_total_file_count += root_files.total_file_count;
                first.estimated_total_bytes += root_files.total_bytes;
            }
            None => chunks.push(PlanChunk {
                chunk_id: "0".to_string(),
                folders: Vec::new(),
                includes_root_files: true,
                estimated_audio_file_count: root_files.audio_file_count,
                estimated_audio_bytes: root_files.audio_bytes,
                estimated_total_file_count: root_files.total_file_count,
                estimated_total_bytes: root_files.total_bytes,
            }),
        }
    }

    BackfillPlan {
        plan_id,
        library_root: shape.root.clone(),
        created_at,
        chunk_folders,
        chunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::shape::SubtreeShape;
    use std::path::PathBuf;

    fn subtree(name: &str, audio: u64, bytes: u64) -> SubtreeShape {
        SubtreeShape {
            relative_path: Some(name.to_string()),
            audio_file_count: audio,
            audio_bytes: bytes,
            total_file_count: audio,
            total_bytes: bytes,
            max_depth: 1,
        }
    }

    #[test]
    fn one_folder_per_chunk_by_default() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![
                subtree("Artist A", 10, 1000),
                subtree("Artist B", 5, 500),
                subtree("Artist C", 20, 2000),
            ],
            root_files: None,
            audio_file_count: 35,
            audio_bytes: 3500,
            total_file_count: 35,
            total_bytes: 3500,
            max_depth: 1,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 1);
        assert_eq!(plan.chunks.len(), 3);
        assert_eq!(plan.chunks[0].folders, vec!["Artist A"]);
        assert_eq!(plan.chunks[0].estimated_audio_file_count, 10);
        assert_eq!(plan.chunks[1].folders, vec!["Artist B"]);
        assert_eq!(plan.chunks[2].folders, vec!["Artist C"]);
        // Ordinal chunk ids, not derived from folder names.
        assert_eq!(plan.chunks[2].chunk_id, "2");
    }

    #[test]
    fn grouping_lever_bundles_n_folders_per_chunk() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![
                subtree("A", 1, 100),
                subtree("B", 2, 200),
                subtree("C", 3, 300),
                subtree("D", 4, 400),
                subtree("E", 5, 500),
            ],
            root_files: None,
            audio_file_count: 15,
            audio_bytes: 1500,
            total_file_count: 15,
            total_bytes: 1500,
            max_depth: 1,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 2);
        assert_eq!(plan.chunks.len(), 3);
        assert_eq!(plan.chunks[0].folders, vec!["A", "B"]);
        assert_eq!(plan.chunks[0].estimated_audio_file_count, 3);
        assert_eq!(plan.chunks[1].folders, vec!["C", "D"]);
        assert_eq!(plan.chunks[2].folders, vec!["E"], "a trailing partial group is still its own chunk");
    }

    #[test]
    fn root_loose_files_fold_into_the_first_chunk_not_a_synthetic_one() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![subtree("Artist A", 10, 1000), subtree("Artist B", 5, 500)],
            root_files: Some(SubtreeShape {
                relative_path: None,
                audio_file_count: 2,
                audio_bytes: 200,
                total_file_count: 3,
                total_bytes: 300,
                max_depth: 0,
            }),
            audio_file_count: 17,
            audio_bytes: 1700,
            total_file_count: 18,
            total_bytes: 1800,
            max_depth: 1,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 1);
        assert_eq!(plan.chunks.len(), 2, "no extra chunk invented for loose files");
        assert!(plan.chunks[0].includes_root_files);
        assert!(!plan.chunks[1].includes_root_files);
        assert_eq!(plan.chunks[0].estimated_audio_file_count, 12);
        assert_eq!(plan.chunks[0].estimated_total_file_count, 13);
    }

    #[test]
    fn root_loose_files_alone_still_produce_a_chunk_even_with_no_subdirectories() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![],
            root_files: Some(SubtreeShape {
                relative_path: None,
                audio_file_count: 4,
                audio_bytes: 400,
                total_file_count: 4,
                total_bytes: 400,
                max_depth: 0,
            }),
            audio_file_count: 4,
            audio_bytes: 400,
            total_file_count: 4,
            total_bytes: 400,
            max_depth: 0,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 1);
        assert_eq!(plan.chunks.len(), 1);
        assert!(plan.chunks[0].includes_root_files);
        assert!(plan.chunks[0].folders.is_empty());
    }

    #[test]
    fn empty_library_produces_an_empty_plan() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![],
            root_files: None,
            audio_file_count: 0,
            audio_bytes: 0,
            total_file_count: 0,
            total_bytes: 0,
            max_depth: 0,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 1);
        assert!(plan.chunks.is_empty());
    }

    #[test]
    fn chunk_folders_of_zero_is_treated_as_one() {
        let shape = LibraryShape {
            root: PathBuf::from("/lib"),
            subtrees: vec![subtree("A", 1, 100), subtree("B", 1, 100)],
            root_files: None,
            audio_file_count: 2,
            audio_bytes: 200,
            total_file_count: 2,
            total_bytes: 200,
            max_depth: 1,
        };
        let plan = build_plan("p1".into(), "2026-01-01T00:00:00Z".into(), &shape, 0);
        assert_eq!(plan.chunk_folders, 1);
        assert_eq!(plan.chunks.len(), 2);
    }
}
