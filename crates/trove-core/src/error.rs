//! Error and result types shared across the whole core.

use thiserror::Error;

/// Convenience alias used throughout `trove-core`.
pub type Result<T> = std::result::Result<T, Error>;

/// Every fallible core operation funnels into this type so callers (CLI, daemon)
/// have one error surface to translate.
#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    /// The local cache is missing/stale and could not be reconciled with the
    /// bucket (and no offline fallback was permitted).
    #[error("reconcile error: {0}")]
    Reconcile(String),

    /// An object store (S3) operation failed.
    #[error("object store error: {0}")]
    Store(String),

    /// A requested entity was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation is recognized but not yet wired up in this bootstrap.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }

    pub fn store(msg: impl Into<String>) -> Self {
        Error::Store(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }
}
