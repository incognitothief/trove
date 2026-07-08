//! Audio metadata extraction.
//!
//! Defined as a trait so the expensive, format-specific extraction (planned via
//! `lofty`/`symphonia`) can be swapped and mocked. The bootstrap ships a
//! filename-based [`StubExtractor`] good enough to exercise import/query flows.

use std::path::Path;

use crate::error::Result;
use crate::model::Metadata;

/// Extracts [`Metadata`] from an audio file on disk.
pub trait MetadataExtractor: Send + Sync {
    fn extract(&self, path: &Path) -> Result<Metadata>;
}

/// Placeholder extractor: infers only file type and a title from the filename.
#[derive(Default)]
pub struct StubExtractor;

impl MetadataExtractor for StubExtractor {
    fn extract(&self, path: &Path) -> Result<Metadata> {
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        let file_type = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        Ok(Metadata {
            title,
            file_type,
            ..Default::default()
        })
    }
}
