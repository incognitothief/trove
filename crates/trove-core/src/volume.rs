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

/// Initialize a volume at `mount_point`: create the standard layout, write
/// an identity file, and open the host-side volume DB when `trove_home` is set.
pub fn init(
    mount_point: &Path,
    label: Option<String>,
    trove_home: Option<&Path>,
) -> Result<VolumeIdentity> {
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
        if let Some(home) = trove_home.filter(|h| *h != Path::new(":memory:")) {
            let db_path = volume_db_path(home, &existing.volume_id);
            if !db_path.exists() {
                let db = crate::db::volume::VolumeDb::open(&db_path)?;
                db.set_meta("volume_id", &existing.volume_id)?;
                if let Some(ref lbl) = existing.label {
                    db.set_meta("label", lbl)?;
                }
                db.set_meta("mount_point", &mount_point.display().to_string())?;
            }
        }
        return Ok(existing);
    }
    let identity = VolumeIdentity::new(label);
    std::fs::write(&identity_path, serde_json::to_vec_pretty(&identity)?)?;

    if let Some(home) = trove_home.filter(|h| *h != Path::new(":memory:")) {
        let db_path = volume_db_path(home, &identity.volume_id);
        let db = crate::db::volume::VolumeDb::open(&db_path)?;
        db.set_meta("volume_id", &identity.volume_id)?;
        if let Some(ref lbl) = identity.label {
            db.set_meta("label", lbl)?;
        }
        db.set_meta("mount_point", &mount_point.display().to_string())?;
    }

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

/// List known volumes from host-side DB files.
pub fn list_volumes(trove_home: &Path) -> Result<Vec<VolumeIdentity>> {
    let dir = trove_home.join("volumes");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sqlite") {
            continue;
        }
        let db = crate::db::volume::VolumeDb::open(&path)?;
        let volume_id = db
            .get_meta("volume_id")?
            .unwrap_or_else(|| path.file_stem().unwrap().to_string_lossy().into());
        let label = db.get_meta("label")?;
        out.push(VolumeIdentity {
            volume_id,
            label,
            created_at: chrono::Utc::now(),
        });
    }
    out.sort_by(|a, b| a.volume_id.cmp(&b.volume_id));
    Ok(out)
}

/// Path to the per-volume database within `~/.trove/volumes`.
pub fn volume_db_path(trove_home: &Path, volume_id: &str) -> PathBuf {
    trove_home
        .join("volumes")
        .join(format!("{volume_id}.sqlite"))
}

/// Open the host-side DB for a mounted volume.
pub fn open_volume_db(
    trove_home: &Path,
    identity: &VolumeIdentity,
) -> Result<crate::db::volume::VolumeDb> {
    crate::db::volume::VolumeDb::open(&volume_db_path(trove_home, &identity.volume_id))
}
