//! Removable performance volumes.
//!
//! The mounted drive stays boring and portable: a `Music/` tree, a `Playlists/`
//! tree, and a tiny `.trove-volume.json` identity file. It must never carry the
//! `.trove` database folder. Per-volume state lives host-side in
//! `~/.trove/volumes/{volume_id}.sqlite`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::VolumeIdentity;

/// The identity filename written at a volume root.
pub const VOLUME_IDENTITY_FILE: &str = ".trove-volume.json";

/// Initialize a volume at `mount_point`: create the standard layout and write
/// an identity file so the drive is recognizable across mounts.
pub fn init(mount_point: &Path, label: Option<String>) -> Result<VolumeIdentity> {
    if !mount_point.exists() {
        return Err(Error::not_found(format!(
            "mount point {}",
            mount_point.display()
        )));
    }
    std::fs::create_dir_all(mount_point.join("Music"))?;
    std::fs::create_dir_all(mount_point.join("Playlists"))?;

    let identity_path = mount_point.join(VOLUME_IDENTITY_FILE);
    if let Some(existing) = read_identity(mount_point)? {
        return Ok(existing);
    }
    let identity = VolumeIdentity::new(label);
    std::fs::write(&identity_path, serde_json::to_vec_pretty(&identity)?)?;
    Ok(identity)
}

/// Read a volume's identity file, if present.
pub fn read_identity(mount_point: &Path) -> Result<Option<VolumeIdentity>> {
    let path = mount_point.join(VOLUME_IDENTITY_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Path to the per-volume database within `~/.trove/volumes`.
pub fn volume_db_path(trove_home: &Path, volume_id: &str) -> PathBuf {
    trove_home
        .join("volumes")
        .join(format!("{volume_id}.sqlite"))
}
