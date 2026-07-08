//! Object store abstraction (the bucket).
//!
//! Trove never talks to S3 directly from clients; instead the core depends on
//! this trait. The production implementation will wrap `aws-sdk-s3` (with
//! multipart upload + staging-prefix support per ADR "S3 upload specifics").
//! A [`stub::StubStore`] is provided so the rest of the core can be exercised
//! without network or credentials during bootstrap.

pub mod fs;
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
