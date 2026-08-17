//! Chunk-level stat sanity check (ADR 007, Group E4).
//!
//! Folder names can coincidentally collide across genuinely different
//! content — the one real risk case identified while stress-testing this
//! design: a machine's drive having a same-named folder with unrelated
//! contents underneath. The shape scan (Group E1) already recorded a rough
//! audio file count and byte total for each chunk in the Plan document
//! (Group E2); this compares that estimate against what actually got
//! scanned at claim time. A large mismatch is a cheap, `stat()`-only signal
//! that something doesn't match what was originally scoped — **not a hard
//! guarantee**, a proportionate sanity check using data already collected
//! for free, consistent with this whole layer's role as a coarse,
//! best-effort hint rather than a correctness boundary. It never blocks a
//! claim from completing.

use serde::{Deserialize, Serialize};

/// A chunk is flagged if either its file count or its byte total differs
/// from the shape scan's estimate by at least this factor, in either
/// direction. Deliberately coarse: this should stay quiet on ordinary drift
/// (a few files added or removed since the plan was made) and only fire on
/// something substantially different underneath, matching the ADR's own
/// illustrative example of a chunk recorded as ~8 tracks completing with
/// ~40 underneath.
const MISMATCH_FACTOR: f64 = 2.0;

/// The result of comparing one chunk's shape-scan estimate against what
/// actually got scanned when it was claimed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StatSanityCheck {
    pub estimated_audio_file_count: u64,
    pub estimated_audio_bytes: u64,
    pub actual_audio_file_count: u64,
    pub actual_audio_bytes: u64,
    /// Whether either count or byte total differs from its estimate by at
    /// least [`MISMATCH_FACTOR`]. A hint for the operator, not a failure —
    /// nothing in this crate refuses a claim because of it.
    pub mismatch: bool,
}

pub fn check_chunk_stats(
    estimated_audio_file_count: u64,
    estimated_audio_bytes: u64,
    actual_audio_file_count: u64,
    actual_audio_bytes: u64,
) -> StatSanityCheck {
    let mismatch = ratio_exceeds(estimated_audio_file_count, actual_audio_file_count)
        || ratio_exceeds(estimated_audio_bytes, actual_audio_bytes);
    StatSanityCheck {
        estimated_audio_file_count,
        estimated_audio_bytes,
        actual_audio_file_count,
        actual_audio_bytes,
        mismatch,
    }
}

fn ratio_exceeds(estimated: u64, actual: u64) -> bool {
    match (estimated, actual) {
        (0, 0) => false,
        // One side is zero and the other isn't -- always a mismatch,
        // dividing would be meaningless.
        (0, _) | (_, 0) => true,
        (estimated, actual) => {
            let ratio = actual as f64 / estimated as f64;
            ratio >= MISMATCH_FACTOR || ratio <= 1.0 / MISMATCH_FACTOR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_match_is_not_a_mismatch() {
        let check = check_chunk_stats(212, 7_000_000_000, 212, 7_000_000_000);
        assert!(!check.mismatch);
    }

    #[test]
    fn zero_estimated_and_zero_actual_is_not_a_mismatch() {
        let check = check_chunk_stats(0, 0, 0, 0);
        assert!(!check.mismatch);
    }

    #[test]
    fn ordinary_drift_is_not_flagged() {
        // A handful of files added/removed since the plan was made --
        // exactly the kind of everyday drift this must stay quiet on.
        let check = check_chunk_stats(212, 7_000_000_000, 215, 7_100_000_000);
        assert!(!check.mismatch);
    }

    #[test]
    fn the_adrs_own_illustrative_mismatch_is_flagged() {
        // "a chunk recorded as 8 tracks / ~45MB completing with 40 tracks /
        // ~300MB underneath" -- the exact motivating example from the ADR.
        let check = check_chunk_stats(8, 45_000_000, 40, 300_000_000);
        assert!(check.mismatch);
    }

    #[test]
    fn a_count_mismatch_alone_is_enough_even_if_bytes_roughly_agree() {
        let check = check_chunk_stats(10, 1_000_000, 25, 1_000_000);
        assert!(check.mismatch, "count differs by 2.5x even though bytes match");
    }

    #[test]
    fn a_byte_mismatch_alone_is_enough_even_if_count_matches() {
        let check = check_chunk_stats(10, 1_000_000, 10, 5_000_000);
        assert!(check.mismatch, "bytes differ by 5x even though count matches");
    }

    #[test]
    fn zero_estimated_with_any_actual_content_is_a_mismatch() {
        // The loose-root-files-only case: a chunk that scoped no audio at
        // all but actually found some underneath.
        let check = check_chunk_stats(0, 0, 3, 1_000);
        assert!(check.mismatch);
    }

    #[test]
    fn actual_content_disappearing_entirely_is_also_a_mismatch() {
        let check = check_chunk_stats(10, 1_000_000, 0, 0);
        assert!(check.mismatch);
    }

    #[test]
    fn the_boundary_ratio_itself_counts_as_a_mismatch() {
        // Exactly 2x is ">=", not just ">" -- confirm the boundary is
        // inclusive, not accidentally off by one.
        let check = check_chunk_stats(10, 1, 20, 1);
        assert!(check.mismatch);
    }
}
