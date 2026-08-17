//! Claim policy and chunk-target resolution for `library plan claim` (ADR
//! 007, Group E3a).
//!
//! Kept pure and separate from the actual event-writing/import-driving in
//! `facade.rs` so the two policy decisions this unit has to get right —
//! *which* chunk gets picked next, and *which real paths* a chunk resolves
//! to — are unit-testable without touching a store or the filesystem.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::library::{BackfillPlan, ChunkState, ChunkStatus, PlanChunk};

/// Outcome of a single `library plan claim` invocation.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimReport {
    pub plan_id: String,
    pub chunk_id: String,
    /// Every path the chunk resolved to and was actually imported, one
    /// ordinary `import plan → run → commit` job each.
    pub targets: Vec<PathBuf>,
    pub tracks_committed: u64,
}

/// Why `pick_next_chunk` couldn't resolve a chunk to claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickNextError {
    /// An explicit chunk id was given, but no chunk in the plan has it.
    UnknownChunkId(String),
    /// No explicit chunk id was given, every chunk has at least one
    /// `claimed` or `completed` event, and `--include-claimed` wasn't
    /// passed. This is the "no steal by default" guard: picking up a
    /// possibly-still-in-progress chunk needs an explicit opt-in.
    NoUntouchedChunksRemain,
    /// No explicit chunk id was given, `--include-claimed` was passed, but
    /// every chunk is already `completed` — there's nothing left to redo.
    AllChunksCompleted,
}

/// Claim policy, decided explicitly (ADR 007, Group E3a): default-on but
/// **non-exclusive, no TTL, nothing ever auto-steals a claim.**
///
/// - An explicit `chunk_id` is always honored as-is (still validated against
///   the plan) — claiming a specific chunk never needs a policy decision.
/// - With no `chunk_id`: prefer a chunk with no events at all, in the
///   plan's listed order (the shape scan's alphabetical folder order).
/// - If none remain untouched and `include_claimed` is set: fall back to
///   the first not-yet-`completed` chunk (a stale claim), still in plan
///   order — an explicit opt-in, never the default.
/// - Otherwise: refuse. Silently redoing another machine's possibly-still-
///   in-progress work is exactly what this default exists to prevent.
pub fn pick_next_chunk(
    plan: &BackfillPlan,
    statuses: &[ChunkStatus],
    explicit_chunk_id: Option<&str>,
    include_claimed: bool,
) -> Result<String, PickNextError> {
    if let Some(id) = explicit_chunk_id {
        return if plan.chunks.iter().any(|c| c.chunk_id == id) {
            Ok(id.to_string())
        } else {
            Err(PickNextError::UnknownChunkId(id.to_string()))
        };
    }

    if let Some(status) = statuses.iter().find(|s| s.state == ChunkState::Untouched) {
        return Ok(status.chunk_id.clone());
    }

    if include_claimed {
        return statuses
            .iter()
            .find(|s| s.state != ChunkState::Completed)
            .map(|s| s.chunk_id.clone())
            .ok_or(PickNextError::AllChunksCompleted);
    }

    Err(PickNextError::NoUntouchedChunksRemain)
}

/// Resolve a chunk's folders (relative-path slugs) to real, absolute
/// filesystem paths under `library_root`, plus — if the chunk covers the
/// library root's loose files — whatever `loose_root_files` the caller
/// already discovered (a fresh `readdir` of the root, since the shape scan
/// that produced the plan only recorded aggregate stats for them, not the
/// individual paths; the caller is expected to redo that small listing at
/// claim time, matching Group E1's own filtering).
///
/// Each returned path is an independent target for the ordinary `import
/// plan → run → commit` pipeline — one job per folder, since a
/// multi-folder chunk's folders are siblings with no common parent that
/// doesn't also pull in other chunks' folders.
pub fn resolve_chunk_targets(
    library_root: &Path,
    chunk: &PlanChunk,
    loose_root_files: &[PathBuf],
) -> Vec<PathBuf> {
    let mut targets: Vec<PathBuf> = chunk
        .folders
        .iter()
        .map(|folder| library_root.join(folder))
        .collect();
    if chunk.includes_root_files {
        targets.extend(loose_root_files.iter().cloned());
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::events::ChunkEvent;
    use crate::library::{ChunkEventKind, PlanChunk};

    fn plan_with_chunks(chunk_ids: &[&str]) -> BackfillPlan {
        BackfillPlan {
            plan_id: "p1".to_string(),
            library_root: PathBuf::from("/lib"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            chunk_folders: 1,
            chunks: chunk_ids
                .iter()
                .map(|id| PlanChunk {
                    chunk_id: id.to_string(),
                    folders: vec![id.to_string()],
                    includes_root_files: false,
                    estimated_audio_file_count: 1,
                    estimated_audio_bytes: 1,
                    estimated_total_file_count: 1,
                    estimated_total_bytes: 1,
                })
                .collect(),
        }
    }

    fn status(chunk_id: &str, state: ChunkState) -> ChunkStatus {
        let event = |kind| ChunkEvent {
            event_id: "e".to_string(),
            chunk_id: chunk_id.to_string(),
            kind,
            by: "someone".to_string(),
            at: "t0".to_string(),
        };
        ChunkStatus {
            chunk_id: chunk_id.to_string(),
            state,
            claims: if state == ChunkState::Untouched {
                vec![]
            } else {
                vec![event(ChunkEventKind::Claimed)]
            },
            completions: if state == ChunkState::Completed {
                vec![event(ChunkEventKind::Completed)]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn an_explicit_chunk_id_is_always_honored_regardless_of_status() {
        let plan = plan_with_chunks(&["0", "1"]);
        let statuses = vec![
            status("0", ChunkState::Completed),
            status("1", ChunkState::Untouched),
        ];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, Some("0"), false),
            Ok("0".to_string())
        );
    }

    #[test]
    fn an_explicit_unknown_chunk_id_is_rejected() {
        let plan = plan_with_chunks(&["0"]);
        let statuses = vec![status("0", ChunkState::Untouched)];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, Some("nope"), false),
            Err(PickNextError::UnknownChunkId("nope".to_string()))
        );
    }

    #[test]
    fn prefers_the_first_untouched_chunk_in_plan_order() {
        let plan = plan_with_chunks(&["0", "1", "2"]);
        let statuses = vec![
            status("0", ChunkState::Completed),
            status("1", ChunkState::Untouched),
            status("2", ChunkState::Untouched),
        ];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, None, false),
            Ok("1".to_string())
        );
    }

    #[test]
    fn refuses_when_nothing_is_untouched_and_include_claimed_is_not_set() {
        let plan = plan_with_chunks(&["0", "1"]);
        let statuses = vec![
            status("0", ChunkState::Completed),
            status("1", ChunkState::Claimed),
        ];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, None, false),
            Err(PickNextError::NoUntouchedChunksRemain)
        );
    }

    #[test]
    fn include_claimed_picks_the_first_not_yet_completed_chunk() {
        let plan = plan_with_chunks(&["0", "1"]);
        let statuses = vec![
            status("0", ChunkState::Completed),
            status("1", ChunkState::Claimed),
        ];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, None, true),
            Ok("1".to_string())
        );
    }

    #[test]
    fn include_claimed_refuses_only_when_truly_everything_is_completed() {
        let plan = plan_with_chunks(&["0", "1"]);
        let statuses = vec![
            status("0", ChunkState::Completed),
            status("1", ChunkState::Completed),
        ];
        assert_eq!(
            pick_next_chunk(&plan, &statuses, None, true),
            Err(PickNextError::AllChunksCompleted)
        );
    }

    #[test]
    fn resolves_folders_to_absolute_paths_under_the_library_root() {
        let chunk = PlanChunk {
            chunk_id: "0".to_string(),
            folders: vec!["Artist A".to_string(), "Artist B".to_string()],
            includes_root_files: false,
            estimated_audio_file_count: 0,
            estimated_audio_bytes: 0,
            estimated_total_file_count: 0,
            estimated_total_bytes: 0,
        };
        let targets = resolve_chunk_targets(Path::new("/lib"), &chunk, &[]);
        assert_eq!(
            targets,
            vec![PathBuf::from("/lib/Artist A"), PathBuf::from("/lib/Artist B")]
        );
    }

    #[test]
    fn includes_loose_root_files_only_when_the_chunk_covers_them() {
        let loose = vec![PathBuf::from("/lib/loose.mp3")];

        let with_root_files = PlanChunk {
            chunk_id: "0".to_string(),
            folders: vec!["Artist A".to_string()],
            includes_root_files: true,
            estimated_audio_file_count: 0,
            estimated_audio_bytes: 0,
            estimated_total_file_count: 0,
            estimated_total_bytes: 0,
        };
        let targets = resolve_chunk_targets(Path::new("/lib"), &with_root_files, &loose);
        assert_eq!(
            targets,
            vec![PathBuf::from("/lib/Artist A"), PathBuf::from("/lib/loose.mp3")]
        );

        let without_root_files = PlanChunk {
            includes_root_files: false,
            ..with_root_files
        };
        let targets = resolve_chunk_targets(Path::new("/lib"), &without_root_files, &loose);
        assert_eq!(targets, vec![PathBuf::from("/lib/Artist A")]);
    }
}
