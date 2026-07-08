//! Client-side wiring: locate config, pick an object store, open core.
//!
//! Mirrors the CLI's wiring. No domain logic — just constructs a [`Trove`].
//! During bootstrap the "bucket" is a local directory (see [`FsStore`]).

use std::path::PathBuf;

use anyhow::{Context, Result};
use trove_core::config::{self, Config};
use trove_core::store::fs::FsStore;
use trove_core::Trove;

pub fn trove_home() -> Result<PathBuf> {
    Ok(config::trove_home()?)
}

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

pub fn bucket_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("TROVE_BUCKET_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(trove_home()?.join("bucket-sim"))
}

pub fn open_trove() -> Result<Trove> {
    let config = load_config()?;
    let home = trove_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let store = Box::new(FsStore::new(bucket_dir()?));
    Trove::open_with_store(config, &home, store).context("opening trove-core")
}
