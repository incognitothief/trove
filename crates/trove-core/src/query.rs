//! Query specification for searching the archive index.
//!
//! A [`QuerySpec`] is a declarative filter; the archive database compiles it to
//! SQL. Clients (CLI/UI) build these and never construct SQL themselves.

use serde::{Deserialize, Serialize};

/// Inclusive numeric range, e.g. BPM `118..=124`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Range<T> {
    #[serde(default = "none")]
    pub min: Option<T>,
    #[serde(default = "none")]
    pub max: Option<T>,
}

fn none<T>() -> Option<T> {
    None
}

impl<T> Default for Range<T> {
    fn default() -> Self {
        Range {
            min: None,
            max: None,
        }
    }
}

/// A declarative query over the archive index.
///
/// Every field is optional on the wire (`#[serde(default)]`) so clients can send
/// only the filters they care about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuerySpec {
    /// Free-text match across title/artist/album/comment.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub file_type: Option<String>,
    #[serde(default)]
    pub bpm: Range<f32>,
    #[serde(default)]
    pub year: Range<i32>,
    /// Tags that must all be present.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl QuerySpec {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = Some(t.into());
        self
    }

    pub fn artist(mut self, a: impl Into<String>) -> Self {
        self.artist = Some(a.into());
        self
    }

    pub fn genre(mut self, g: impl Into<String>) -> Self {
        self.genre = Some(g.into());
        self
    }

    pub fn key(mut self, k: impl Into<String>) -> Self {
        self.key = Some(k.into());
        self
    }

    pub fn bpm_range(mut self, min: Option<f32>, max: Option<f32>) -> Self {
        self.bpm = Range { min, max };
        self
    }
}
