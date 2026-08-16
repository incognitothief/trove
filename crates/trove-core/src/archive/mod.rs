//! The canonical bucket-side archive index and its reconciliation.
//!
//! S3 is the only source of truth, but S3 is not directly queryable, so we keep
//! a portable index in the bucket: an immutable, generation-keyed JSONL object
//! (`archive-index/<generation>.jsonl`) plus a `schema-version.json` marker
//! that points at the current one. The marker is the only mutable object and
//! is CAS-protected (ADR 007, Group B1) — the index payload itself is never
//! overwritten once written, so a conflicting writer can never silently
//! clobber another writer's entries. Reads reconcile the local cache against
//! this index before serving (ADR "Reads reconcile with the bucket").

pub mod index;
pub mod reconcile;
pub mod verify;

pub use index::{BucketPaths, ARCHIVE_INDEX_SQLITE, SCHEMA_VERSION_JSON};
pub use reconcile::{reconcile, ReconcileReport};
pub use verify::{verify_archive, ArchiveVerifyReport};

/// The schema version this build writes and understands.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
