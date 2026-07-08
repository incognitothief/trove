//! Explicit per-file and per-job state for resumable bulk import.
//!
//! ADR rule: *never make success implicit*. Every file carries an explicit
//! state and the canonical index only advances after verify → commit.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ordered, resumable phases of a bulk import job.
///
/// `scan → fingerprint → dedupe → upload → verify → commit`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Scan,
    Fingerprint,
    Dedupe,
    Upload,
    Verify,
    Commit,
    Done,
}

impl Phase {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scan" => Some(Phase::Scan),
            "fingerprint" => Some(Phase::Fingerprint),
            "dedupe" => Some(Phase::Dedupe),
            "upload" => Some(Phase::Upload),
            "verify" => Some(Phase::Verify),
            "commit" => Some(Phase::Commit),
            "done" => Some(Phase::Done),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Scan => "scan",
            Phase::Fingerprint => "fingerprint",
            Phase::Dedupe => "dedupe",
            Phase::Upload => "upload",
            Phase::Verify => "verify",
            Phase::Commit => "commit",
            Phase::Done => "done",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Explicit lifecycle state of a single file within an import job.
///
/// ```text
/// pending → scanning → hashed → duplicate
///                            → uploading → uploaded → verified → committed
///                            → failed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileState {
    Pending,
    Scanning,
    Hashed,
    Duplicate,
    Uploading,
    Uploaded,
    Verified,
    Committed,
    Failed,
}

impl FileState {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(FileState::Pending),
            "scanning" => Some(FileState::Scanning),
            "hashed" => Some(FileState::Hashed),
            "duplicate" => Some(FileState::Duplicate),
            "uploading" => Some(FileState::Uploading),
            "uploaded" => Some(FileState::Uploaded),
            "verified" => Some(FileState::Verified),
            "committed" => Some(FileState::Committed),
            "failed" => Some(FileState::Failed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FileState::Pending => "pending",
            FileState::Scanning => "scanning",
            FileState::Hashed => "hashed",
            FileState::Duplicate => "duplicate",
            FileState::Uploading => "uploading",
            FileState::Uploaded => "uploaded",
            FileState::Verified => "verified",
            FileState::Committed => "committed",
            FileState::Failed => "failed",
        }
    }

    /// Whether this state represents completed, safe-to-skip work on resume.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FileState::Duplicate | FileState::Committed | FileState::Failed
        )
    }
}

impl fmt::Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
