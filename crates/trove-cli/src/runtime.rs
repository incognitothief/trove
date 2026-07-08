//! Client-side wiring only: locate config, pick an object store, open core.
//!
//! No domain logic lives here — this just constructs a [`Trove`] the commands
//! can drive. The store is chosen from config (ADR 003): a "local" bucket uses
//! the filesystem simulator ([`FsStore`]); a real bucket uses the S3 store when
//! this binary is built with the `s3` feature.

use std::path::PathBuf;

use anyhow::{Context, Result};
use trove_core::config::{self, Config};
use trove_core::store::fs::FsStore;
use trove_core::store::ObjectStore;
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

/// Whether the configured bucket is the local sentinel (filesystem simulator)
/// rather than a real remote bucket.
fn is_local_bucket(config: &Config) -> bool {
    config.bucket.region == "local" || config.bucket.name == "local"
}

/// Select an object store from config: the filesystem simulator for a local
/// bucket, or the S3 store for a real one (requires the `s3` feature).
fn build_store(config: &Config) -> Result<Box<dyn ObjectStore>> {
    if is_local_bucket(config) {
        return Ok(Box::new(FsStore::new(bucket_dir()?)));
    }

    #[cfg(feature = "s3")]
    {
        use trove_core::store::s3::S3Store;
        let store = S3Store::new(
            &config.bucket.name,
            &config.bucket.region,
            config.bucket.endpoint.clone(),
        )
        .with_context(|| format!("connecting to S3 bucket '{}'", config.bucket.name))?;
        Ok(Box::new(store))
    }

    #[cfg(not(feature = "s3"))]
    {
        anyhow::bail!(
            "config points at bucket '{}' but this build has no S3 support; \
             rebuild with `--features s3`, or set the bucket region to \"local\" \
             to use the filesystem simulator",
            config.bucket.name
        )
    }
}

/// Open a fully wired [`Trove`] for CLI use.
pub fn open_trove() -> Result<Trove> {
    let config = load_config()?;
    let home = trove_home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let store = build_store(&config)?;
    Trove::open_with_store(config, &home, store).context("opening trove-core")
}
