//! Local configuration (`~/.trove/config.toml`).
//!
//! Config is host-local and disposable like everything but the bucket; it only
//! tells Trove *where* the durable archive lives and how the operator prefers
//! to export.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Fully resolved local configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub bucket: BucketConfig,
    #[serde(default)]
    pub local: LocalConfig,
    #[serde(default)]
    pub export: ExportConfig,
    #[serde(default)]
    pub mixxx: MixxxConfig,
    #[serde(default)]
    pub import: ImportConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// Import-time behavior. See ADR 002.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportConfig {
    /// Include dotfiles / hidden directories in scans (default: exclude).
    #[serde(default)]
    pub include_dotfiles: bool,
    /// Capture co-located cover art during import (default: on). This is a
    /// durability measure — folder art exists only on the disposable source.
    #[serde(default = "default_true")]
    pub capture_artwork: bool,
}

impl Default for ImportConfig {
    fn default() -> Self {
        ImportConfig {
            include_dotfiles: false,
            capture_artwork: true,
        }
    }
}

/// Canonical archive location — the only durable source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketConfig {
    pub name: String,
    pub region: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default = "default_music_prefix")]
    pub music_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalConfig {
    #[serde(default)]
    pub music_folder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConfig {
    #[serde(default = "default_layout")]
    pub layout: ExportLayout,
    #[serde(default = "default_playlist_format")]
    pub playlist_format: PlaylistFormat,
}

impl Default for ExportConfig {
    fn default() -> Self {
        ExportConfig {
            layout: default_layout(),
            playlist_format: default_playlist_format(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportLayout {
    /// `Music/Artist/Album/Track.flac`
    ArtistAlbum,
    /// `Music/Track.flac`
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaylistFormat {
    M3u8,
    M3u,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixxxConfig {
    #[serde(default = "default_true")]
    pub relative_paths: bool,
}

impl Default for MixxxConfig {
    fn default() -> Self {
        MixxxConfig {
            relative_paths: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub bucket: String,
    pub region: String,
    #[serde(default)]
    pub endpoint: Option<String>,
}

fn default_prefix() -> String {
    ".trove".to_string()
}
fn default_music_prefix() -> String {
    "music".to_string()
}
fn default_layout() -> ExportLayout {
    ExportLayout::ArtistAlbum
}
fn default_playlist_format() -> PlaylistFormat {
    PlaylistFormat::M3u8
}
fn default_true() -> bool {
    true
}

impl Config {
    /// Parse configuration from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::config(format!("could not read config at {}: {e}", path.display()))
        })?;
        Self::from_toml(&text)
    }

    /// Load configuration from the default location (`~/.trove/config.toml`).
    pub fn load_default() -> Result<Self> {
        Self::load_from(&default_config_path()?)
    }
}

/// Resolve `~/.trove`, the operational cache/config root.
pub fn trove_home() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("TROVE_HOME") {
        return Ok(PathBuf::from(explicit));
    }
    let home =
        dirs::home_dir().ok_or_else(|| Error::config("could not determine home directory"))?;
    Ok(home.join(".trove"))
}

/// Default config path (`~/.trove/config.toml`).
pub fn default_config_path() -> Result<PathBuf> {
    Ok(trove_home()?.join("config.toml"))
}
