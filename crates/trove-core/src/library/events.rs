//! Append-only event log for chunk progress (ADR 007, Group E3).
//!
//! Progress against a [`crate::library::BackfillPlan`] is tracked as small,
//! independently-keyed event objects appended under the plan — never by
//! mutating a shared document. Each event is a pure create with a unique
//! key, so unlike `schema-version.json` this coordination layer needs
//! **zero** compare-and-swap: two machines can never race on the same
//! write, because they never write the same key. Any machine (including one
//! joining later) can reconstruct current status by pulling the plan once
//! and folding its event log — that's [`fold_chunk_status`] below.
//!
//! Claiming is deliberately non-exclusive: two machines redundantly working
//! the same chunk is wasteful, not incorrect (content-addressing and the
//! Group D2/D3 fast path make the redundant work cheap to absorb), so this
//! module makes no attempt to prevent it. It only records what happened.

use serde::{Deserialize, Serialize};

/// What a [`ChunkEvent`] records happening to a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkEventKind {
    /// A machine started working this chunk. Optional but default-on in
    /// practice — it costs one small write and is the only thing that gives
    /// a second machine real-time visibility to avoid picking the same
    /// chunk. Not a lock: nothing is ever blocked by it.
    Claimed,
    /// A chunk's ordinary `import plan → run → commit` pipeline finished
    /// successfully.
    Completed,
}

/// One append-only event against a specific chunk of a specific plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkEvent {
    /// Unique key within the plan's event log — this, not `chunk_id`, is
    /// what the object is stored under, which is what makes every write a
    /// pure create.
    pub event_id: String,
    pub chunk_id: String,
    pub kind: ChunkEventKind,
    /// Free-form identifier for whoever recorded this event (hostname, an
    /// operator-supplied label — whatever the caller passes). Purely
    /// informational: nothing here is used to grant or deny a claim.
    pub by: String,
    /// RFC 3339 timestamp of when this event was recorded.
    pub at: String,
}

/// A chunk's current status, reconstructed by folding a plan's event log.
/// "Current" is a snapshot as of whatever events had been read — a claim
/// with no matching completion is a stale-claim *hint*, not a guarantee the
/// chunk isn't still being worked, or even that it is.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkStatus {
    pub chunk_id: String,
    pub state: ChunkState,
    /// Every `Claimed` event seen for this chunk, oldest first.
    pub claims: Vec<ChunkEvent>,
    /// Every `Completed` event seen for this chunk, oldest first. Normally
    /// zero or one, but nothing prevents more than one if two machines
    /// independently finished the same chunk — that's redundant work, not
    /// an error, consistent with claiming being non-exclusive.
    pub completions: Vec<ChunkEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkState {
    Untouched,
    Claimed,
    Completed,
}

/// Fold a plan's chunk list and its event log into per-chunk status, in the
/// plan's own chunk order. Events for a `chunk_id` not present in the plan
/// are ignored rather than erroring — a plan is immutable, so this can only
/// happen from a corrupted or hand-edited event, not a normal race.
pub fn fold_chunk_status(
    plan: &crate::library::BackfillPlan,
    events: &[ChunkEvent],
) -> Vec<ChunkStatus> {
    plan.chunks
        .iter()
        .map(|chunk| {
            let mut claims: Vec<ChunkEvent> = events
                .iter()
                .filter(|e| e.chunk_id == chunk.chunk_id && e.kind == ChunkEventKind::Claimed)
                .cloned()
                .collect();
            claims.sort_by(|a, b| a.at.cmp(&b.at));

            let mut completions: Vec<ChunkEvent> = events
                .iter()
                .filter(|e| e.chunk_id == chunk.chunk_id && e.kind == ChunkEventKind::Completed)
                .cloned()
                .collect();
            completions.sort_by(|a, b| a.at.cmp(&b.at));

            let state = if !completions.is_empty() {
                ChunkState::Completed
            } else if !claims.is_empty() {
                ChunkState::Claimed
            } else {
                ChunkState::Untouched
            };

            ChunkStatus {
                chunk_id: chunk.chunk_id.clone(),
                state,
                claims,
                completions,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::plan::{BackfillPlan, PlanChunk};

    fn plan_with_chunks(chunk_ids: &[&str]) -> BackfillPlan {
        BackfillPlan {
            plan_id: "p1".to_string(),
            library_root: std::path::PathBuf::from("/lib"),
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

    fn event(chunk_id: &str, kind: ChunkEventKind, by: &str, at: &str) -> ChunkEvent {
        ChunkEvent {
            event_id: format!("{chunk_id}-{by}-{at}"),
            chunk_id: chunk_id.to_string(),
            kind,
            by: by.to_string(),
            at: at.to_string(),
        }
    }

    #[test]
    fn a_chunk_with_no_events_is_untouched() {
        let plan = plan_with_chunks(&["0", "1"]);
        let statuses = fold_chunk_status(&plan, &[]);
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().all(|s| s.state == ChunkState::Untouched));
    }

    #[test]
    fn a_claim_with_no_completion_is_claimed_not_completed() {
        let plan = plan_with_chunks(&["0"]);
        let events = vec![event("0", ChunkEventKind::Claimed, "machine-a", "t1")];
        let statuses = fold_chunk_status(&plan, &events);
        assert_eq!(statuses[0].state, ChunkState::Claimed);
        assert_eq!(statuses[0].claims.len(), 1);
        assert!(statuses[0].completions.is_empty());
    }

    #[test]
    fn a_completion_wins_over_a_claim_regardless_of_event_order() {
        let plan = plan_with_chunks(&["0"]);
        let events = vec![
            event("0", ChunkEventKind::Completed, "machine-a", "t1"),
            event("0", ChunkEventKind::Claimed, "machine-a", "t0"),
        ];
        let statuses = fold_chunk_status(&plan, &events);
        assert_eq!(statuses[0].state, ChunkState::Completed);
    }

    #[test]
    fn redundant_claims_and_completions_from_two_machines_are_all_kept_not_deduped() {
        // Non-exclusive by design: two machines can both claim and both
        // complete the same chunk. That's wasteful, not wrong, and the
        // event log should show all of it, not silently collapse it.
        let plan = plan_with_chunks(&["0"]);
        let events = vec![
            event("0", ChunkEventKind::Claimed, "machine-a", "t0"),
            event("0", ChunkEventKind::Claimed, "machine-b", "t1"),
            event("0", ChunkEventKind::Completed, "machine-a", "t2"),
            event("0", ChunkEventKind::Completed, "machine-b", "t3"),
        ];
        let statuses = fold_chunk_status(&plan, &events);
        assert_eq!(statuses[0].state, ChunkState::Completed);
        assert_eq!(statuses[0].claims.len(), 2);
        assert_eq!(statuses[0].completions.len(), 2);
    }

    #[test]
    fn events_are_scoped_to_their_own_chunk() {
        let plan = plan_with_chunks(&["0", "1"]);
        let events = vec![event("0", ChunkEventKind::Completed, "machine-a", "t0")];
        let statuses = fold_chunk_status(&plan, &events);
        assert_eq!(statuses[0].state, ChunkState::Completed);
        assert_eq!(statuses[1].state, ChunkState::Untouched);
    }

    #[test]
    fn results_are_returned_in_the_plans_own_chunk_order() {
        let plan = plan_with_chunks(&["2", "0", "1"]);
        let statuses = fold_chunk_status(&plan, &[]);
        let ids: Vec<&str> = statuses.iter().map(|s| s.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["2", "0", "1"]);
    }

    #[test]
    fn an_event_for_an_unknown_chunk_id_is_silently_ignored() {
        let plan = plan_with_chunks(&["0"]);
        let events = vec![event(
            "does-not-exist",
            ChunkEventKind::Completed,
            "machine-a",
            "t0",
        )];
        let statuses = fold_chunk_status(&plan, &events);
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, ChunkState::Untouched);
    }
}
