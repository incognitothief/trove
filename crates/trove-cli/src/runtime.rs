//! Client-side wiring only: locate config, pick an object store, open core.
//!
//! No domain logic lives here — this just constructs a [`Trove`] the commands
//! can drive. During bootstrap the "bucket" is a local directory (see
//! [`FsStore`]); swap in the S3-backed store for production.

use std::path::PathBuf;

use anyhow::{Context, Result};
use trove_core::config::{self, Config};
use trove_core::store::fs::FsStore;
use trove_core::Trove;

/// Resolve `~/.trove` (honoring `TROVE_HOME`).
pub fn trove_home() -> Result<PathBuf> {
    Ok(config::trove_home()?)
}

/// Load config from `~/.trove/config.toml`, or synthesize a local-only default
/// so the CLI is usable before the operator writes a real config.
pub fn load_config() -> Result<Config> {
    match Config::load_default() {
        Ok(cfg) => Ok(cfg),
        Err(_) => Config::from_toml(
            r#"
            [bucket]
            name = "local"
            region = "local"
            "#,
        )
        .context("building default config"),
    }
}

/// Directory backing the simulated bucket (`TROVE_BUCKET_DIR`, default
/// `~/.trove/bucket-sim`). The production build replaces this with S3.
pub fn bucket_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("TROVE_BUCKET_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(trove_home()?.join("bucket-sim"))
}

/// Open a fully wired [`Trove`] for CLI use.
pub fn open_trove() -> Result<Trove> {
    let config = load_config()?;
    let home = trove_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let store = Box::new(FsStore::new(bucket_dir()?));
    Trove::open_with_store(config, &home, store).context("opening trove-core")
}
