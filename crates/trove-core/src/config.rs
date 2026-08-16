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
    pub library: LibraryConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// The stable anchor for portable, cross-drive identity (ADR 007, Group D1).
///
/// `ImportJob.source_root` is just whatever path a given `trove import`
/// invocation happened to be pointed at — it isn't stable across differently
/// granular invocations (a whole-tree import vs. a batch import chunked by
/// subdirectory compute different `source_root`s for identical files). The
/// library root is a separate, explicitly declared concept: everything that
/// needs a portable "path relative to the library" (the D2 slug, the E1
/// shape scan, the E2 backfill Plan) is computed relative to *this*, not to
/// whatever a particular command's own arguments happened to be.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibraryConfig {
    #[serde(default)]
    pub root: Option<PathBuf>,
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

/// The local, no-AWS-required default Trove synthesizes in memory when no
/// `config.toml` exists yet (see `trove-cli`'s `runtime::load_config`). Named
/// here, once, so [`set_library_root`] can materialize the *same* default to
/// disk on first write rather than duplicating this literal in a second
/// place where it could silently drift.
pub const LOCAL_DEFAULT_TOML: &str = r#"[bucket]
name = "local"
region = "local"
"#;

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

/// Persist `root` as the declared library root (ADR 007, Group D1) into the
/// config file at `path`, creating the file with the same local-default
/// bucket stanza Trove already synthesizes in memory (see
/// [`LOCAL_DEFAULT_TOML`]) if it doesn't exist yet.
///
/// Uses `toml_edit` rather than round-tripping through `Config`'s own
/// (de)serialization: a full re-serialize would lose comments and silently
/// drop any field this struct doesn't model, on a file that already holds
/// real bucket credentials/settings the operator hand-edited. This only ever
/// touches the one key it's asked to — everything else in the file, byte for
/// byte, is left alone.
pub fn set_library_root(path: &Path, root: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LOCAL_DEFAULT_TOML.to_string(),
        Err(e) => {
            return Err(Error::config(format!(
                "could not read config at {}: {e}",
                path.display()
            )))
        }
    };

    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::config(format!("could not parse {}: {e}", path.display())))?;

    if doc.get("library").and_then(|v| v.as_table()).is_none() {
        doc["library"] = toml_edit::table();
    }
    doc["library"]["root"] = toml_edit::value(root.display().to_string());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_library_root_materializes_local_default_when_no_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(!path.exists());

        set_library_root(&path, Path::new("/Volumes/T7/music/library")).unwrap();

        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            cfg.bucket.name, "local",
            "should get the same local default Trove already synthesizes in memory"
        );
        assert_eq!(
            cfg.library.root,
            Some(PathBuf::from("/Volumes/T7/music/library"))
        );
    }

    #[test]
    fn set_library_root_preserves_existing_content_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"# a hand-written comment the operator cares about
[bucket]
name = "my-real-bucket"
region = "us-east-1"

[export]
layout = "flat"
"#,
        )
        .unwrap();

        set_library_root(&path, Path::new("/Volumes/T7/music/library")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# a hand-written comment the operator cares about"),
            "must not lose comments on an existing config file:\n{text}"
        );
        assert!(text.contains("name = \"my-real-bucket\""));
        assert!(text.contains("layout = \"flat\""));

        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            cfg.bucket.name, "my-real-bucket",
            "unrelated fields must be untouched"
        );
        assert_eq!(cfg.export.layout, ExportLayout::Flat);
        assert_eq!(
            cfg.library.root,
            Some(PathBuf::from("/Volumes/T7/music/library"))
        );
    }

    #[test]
    fn set_library_root_overwrites_a_previously_declared_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        set_library_root(&path, Path::new("/Volumes/T7/music/library")).unwrap();
        set_library_root(&path, Path::new("/Volumes/T72/choons")).unwrap();

        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.library.root, Some(PathBuf::from("/Volumes/T72/choons")));
    }

    #[test]
    fn config_without_a_library_section_parses_with_no_root_declared() {
        let cfg = Config::from_toml(
            r#"
            [bucket]
            name = "test"
            region = "local"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.library.root, None);
    }
}
