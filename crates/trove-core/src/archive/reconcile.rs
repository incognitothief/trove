//! Reconciliation: bring the disposable local cache in line with the bucket.
//!
//! This is the operational heart of "everything but the bucket is disposable":
//! before serving a read, compare the local cache generation against the
//! bucket's `schema-version.json`. If they match, it's a near-instant no-op;
//! otherwise the changed index is pulled and the local cache rehydrated.

use crate::archive::index::{self, BucketPaths};
use crate::db::archive::ArchiveDb;
use crate::error::{Error, Result};
use crate::store::ObjectStore;

/// Outcome of a reconciliation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileReport {
    /// Local cache already matched the bucket generation.
    UpToDate { generation: u64 },
    /// Local cache was (re)hydrated from the bucket.
    Rehydrated {
        generation: u64,
        entries_loaded: usize,
    },
    /// The bucket has no index yet (fresh archive); nothing to pull.
    EmptyBucket,
    /// Reconciliation was skipped and the last cached index will be served.
    OfflineFallback { local_generation: Option<u64> },
}

/// Reconcile `local` against the bucket via `store`.
///
/// When `allow_offline` is true and the bucket is unreachable, the last cached
/// index is accepted (with possible staleness) instead of erroring.
pub fn reconcile(
    store: &dyn ObjectStore,
    paths: &BucketPaths,
    local: &mut ArchiveDb,
    allow_offline: bool,
) -> Result<ReconcileReport> {
    let marker_key = paths.schema_version();

    let remote_marker = match store.get(&marker_key) {
        Ok(bytes) => Some(index::schema_version_from_bytes(&bytes)?),
        Err(Error::NotFound(_)) => None,
        Err(e) => {
            if allow_offline {
                return Ok(ReconcileReport::OfflineFallback {
                    local_generation: local.generation()?,
                });
            }
            return Err(Error::Reconcile(format!(
                "could not read {marker_key}: {e}"
            )));
        }
    };

    let remote_marker = match remote_marker {
        Some(m) => m,
        None => return Ok(ReconcileReport::EmptyBucket),
    };

    let local_generation = local.generation()?;
    if local_generation == Some(remote_marker.generation) && local.count()? > 0 {
        return Ok(ReconcileReport::UpToDate {
            generation: remote_marker.generation,
        });
    }

    // Generations differ (or the cache is empty/cold): pull the JSONL index and
    // rebuild the local cache, then stamp it with the bucket generation.
    let jsonl = store
        .get(&paths.archive_index_jsonl())
        .map_err(|e| Error::Reconcile(format!("could not pull archive index: {e}")))?;
    let entries = index::entries_from_jsonl(&jsonl)?;
    local.replace_all(entries.iter())?;
    local.set_generation(remote_marker.generation)?;

    Ok(ReconcileReport::Rehydrated {
        generation: remote_marker.generation,
        entries_loaded: entries.len(),
    })
}
