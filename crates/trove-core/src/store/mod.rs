//! Object store abstraction (the bucket).
//!
//! Trove never talks to S3 directly from clients; instead the core depends on
//! this trait. Three implementations exist:
//!
//! - [`fs::FsStore`] — a local directory tree simulating the bucket (bootstrap,
//!   no credentials).
//! - [`stub::StubStore`] — in-memory, for tests.
//! - [`s3::S3Store`] — the production `aws-sdk-s3` backend (multipart uploads +
//!   staging→`music/` promotion via server-side copy). Compiled only under the
//!   `s3` feature to keep default builds fast and hermetic (ADR 001, ADR 003).

pub mod fs;
#[cfg(feature = "s3")]
pub mod s3;
pub mod stub;

use crate::error::Result;

/// Metadata about a stored object.
#[derive(Debug, Clone)]
pub struct ObjectMeta {
    pub key: String,
    pub size_bytes: u64,
    pub etag: Option<String>,
}

/// A durable object store (S3 or S3-compatible).
///
/// Kept intentionally small for the bootstrap; multipart/resume specifics will
/// be layered on the production implementation behind these same operations.
pub trait ObjectStore: Send + Sync {
    /// Fetch an object's bytes by key.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// Store bytes at a key, returning the resulting object metadata.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<ObjectMeta>;

    /// Return whether an object exists.
    fn exists(&self, key: &str) -> Result<bool>;

    /// Head an object for metadata without downloading it.
    fn head(&self, key: &str) -> Result<Option<ObjectMeta>>;

    /// Copy an object server-side (used to promote staging → `music/`).
    fn copy(&self, from_key: &str, to_key: &str) -> Result<ObjectMeta>;

    /// List object keys under a prefix.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;
}
