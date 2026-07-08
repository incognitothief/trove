//! The canonical bucket-side archive index and its reconciliation.
//!
//! S3 is the only source of truth, but S3 is not directly queryable, so we keep
//! a portable index in the bucket (`archive-index.jsonl` + `archive-index.sqlite`)
//! plus a `schema-version.json` generation stamp. Reads reconcile the local
//! cache against this index before serving (ADR "Reads reconcile with the bucket").

pub mod index;
pub mod reconcile;

pub use index::{BucketPaths, ARCHIVE_INDEX_JSONL, ARCHIVE_INDEX_SQLITE, SCHEMA_VERSION_JSON};
pub use reconcile::{reconcile, ReconcileReport};

/// The schema version this build writes and understands.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
