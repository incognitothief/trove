//! Filesystem-backed [`ObjectStore`].
//!
//! Simulates a bucket as a local directory tree, giving the CLI and daemon a
//! fully working (persistent) store during bootstrap without AWS credentials.
//! Object keys map directly to relative paths under a root directory.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{ObjectMeta, ObjectStore};
use crate::error::{Error, Result};

/// An object store rooted at a local directory.
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsStore { root: root.into() }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn etag(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }
}

impl ObjectStore for FsStore {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.path_for(key);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::not_found(format!("object {key}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<ObjectMeta> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        Ok(ObjectMeta {
            key: key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::etag(bytes)),
        })
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.path_for(key).exists())
    }

    fn head(&self, key: &str) -> Result<Option<ObjectMeta>> {
        let path = self.path_for(key);
        match std::fs::metadata(&path) {
            Ok(meta) => Ok(Some(ObjectMeta {
                key: key.to_string(),
                size_bytes: meta.len(),
                etag: None,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn copy(&self, from_key: &str, to_key: &str) -> Result<ObjectMeta> {
        let bytes = self.get(from_key)?;
        self.put(to_key, &bytes)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        collect_keys(&self.root, &self.root, &mut keys)?;
        keys.retain(|k| k.starts_with(prefix));
        keys.sort();
        Ok(keys)
    }
}

fn collect_keys(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_keys(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}
