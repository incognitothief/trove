//! Serialization of the bucket-side index interchange formats.
//!
//! The JSONL form is the durable, inspectable interchange format (one
//! [`ArchiveEntry`] per line); the SQLite form exists for fast local restore.

use crate::error::Result;
use crate::model::{ArchiveEntry, ArtworkRecord, SchemaVersion};

/// Object key of the SQLite index (fast restore).
pub const ARCHIVE_INDEX_SQLITE: &str = "archive-index.sqlite";
/// Object key of the reconcile generation marker.
pub const SCHEMA_VERSION_JSON: &str = "schema-version.json";
/// Object key of the playlists interchange file.
pub const PLAYLISTS_JSONL: &str = "playlists.jsonl";
/// Object key (under the bucket prefix) of the artwork provenance manifest.
pub const ARTWORK_MANIFEST_JSONL: &str = "manifests/artwork.jsonl";

/// Resolves fully-qualified object keys under a configured bucket prefix.
#[derive(Debug, Clone)]
pub struct BucketPaths {
    prefix: String,
    music_prefix: String,
}

impl BucketPaths {
    pub fn new(prefix: impl Into<String>, music_prefix: impl Into<String>) -> Self {
        BucketPaths {
            prefix: prefix.into().trim_end_matches('/').to_string(),
            music_prefix: music_prefix.into().trim_end_matches('/').to_string(),
        }
    }

    fn join(&self, name: &str) -> String {
        format!("{}/{}", self.prefix, name)
    }

    /// Object key of the immutable, generation-keyed canonical index
    /// (`archive-index/<generation>.jsonl`). Never overwritten once written —
    /// `schema-version.json` is the only mutable pointer, CAS-protected
    /// separately (ADR 007, Group B1). Derived from the generation number
    /// already carried by [`crate::model::SchemaVersion`]; no separate field
    /// needed to know which object is canonical.
    pub fn archive_index_generation(&self, generation: u64) -> String {
        self.join(&format!("archive-index/{generation}.jsonl"))
    }
    pub fn archive_index_sqlite(&self) -> String {
        self.join(ARCHIVE_INDEX_SQLITE)
    }
    pub fn schema_version(&self) -> String {
        self.join(SCHEMA_VERSION_JSON)
    }
    pub fn playlists_jsonl(&self) -> String {
        self.join(PLAYLISTS_JSONL)
    }
    /// Staging prefix for an in-flight import job (unverified uploads).
    pub fn staging(&self, import_job_id: &str) -> String {
        format!("{}/staging/{}", self.prefix, import_job_id)
    }
    /// Committed, canonical audio namespace.
    pub fn music(&self, relative: &str) -> String {
        format!("{}/{}", self.music_prefix, relative.trim_start_matches('/'))
    }
    /// Content-addressed cover-art namespace (top-level, parallel to music).
    pub fn artwork(&self, relative: &str) -> String {
        format!("artwork/{}", relative.trim_start_matches('/'))
    }
    /// The durable artwork provenance manifest key (under the bucket prefix).
    pub fn artwork_manifest(&self) -> String {
        self.join(ARTWORK_MANIFEST_JSONL)
    }
}

/// Serialize entries to the JSONL interchange format.
pub fn entries_to_jsonl(entries: &[ArchiveEntry]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for entry in entries {
        let line = serde_json::to_string(entry)?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Ok(out)
}

/// Parse entries from the JSONL interchange format, skipping blank lines.
pub fn entries_from_jsonl(bytes: &[u8]) -> Result<Vec<ArchiveEntry>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Serialize artwork provenance records to JSONL.
pub fn artwork_to_jsonl(records: &[ArtworkRecord]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for record in records {
        let line = serde_json::to_string(record)?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Ok(out)
}

/// Parse artwork provenance records from JSONL, skipping blank lines.
pub fn artwork_from_jsonl(bytes: &[u8]) -> Result<Vec<ArtworkRecord>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Serialize the schema-version/generation marker.
pub fn schema_version_bytes(version: &SchemaVersion) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(version)?)
}

/// Parse the schema-version/generation marker.
pub fn schema_version_from_bytes(bytes: &[u8]) -> Result<SchemaVersion> {
    Ok(serde_json::from_slice(bytes)?)
}
