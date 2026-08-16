//! Filesystem-backed [`ObjectStore`].
//!
//! Simulates a bucket as a local directory tree, giving the CLI and daemon a
//! fully working (persistent) store during bootstrap without AWS credentials.
//! Object keys map directly to relative paths under a root directory.

use std::path::{Path, PathBuf};

use super::{ObjectMeta, ObjectStore, PutOutcome};
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

    /// A cheap, stat-only surrogate etag: `(size, mtime)`. Deliberately not a
    /// content hash — `head()` is called on real (potentially large) audio
    /// objects during import verify (`import/mod.rs`), so an etag scheme that
    /// requires reading full file contents would regress that path. `put()`,
    /// `head()`, and `put_if_match()` all use this same scheme so a
    /// `head()`-observed etag is always comparable to what `put_if_match`
    /// checks. Good enough for a single-host dev simulator; not a claim of
    /// true multi-writer atomicity (ADR 007, Group B1).
    fn surrogate_etag(meta: &std::fs::Metadata) -> String {
        let mtime_nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}-{mtime_nanos}", meta.len())
    }

    fn current_etag(&self, path: &Path) -> Option<String> {
        std::fs::metadata(path).ok().map(|m| Self::surrogate_etag(&m))
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
        let meta = std::fs::metadata(&path)?;
        Ok(ObjectMeta {
            key: key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::surrogate_etag(&meta)),
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
                etag: Some(Self::surrogate_etag(&meta)),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Best-effort local CAS: check-then-write with an atomic `rename()` for
    /// the write step itself. The check and the write are not one atomic
    /// operation (no cross-process file lock), so this narrows but does not
    /// eliminate the race window a true multi-writer store must close. That's
    /// an accepted limitation for a single-host dev simulator — see ADR 007,
    /// Group B1, on why real concurrency proofs must run against S3 instead.
    fn put_if_match(
        &self,
        key: &str,
        expected_etag: Option<&str>,
        bytes: &[u8],
    ) -> Result<PutOutcome> {
        let path = self.path_for(key);
        let current = self.current_etag(&path);
        let matches = match expected_etag {
            None => current.is_none(),
            Some(expected) => current.as_deref() == Some(expected),
        };
        if !matches {
            return Ok(PutOutcome::Conflict {
                current_etag: current,
            });
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("object");
        let tmp_path = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
        std::fs::write(&tmp_path, bytes)?;
        std::fs::rename(&tmp_path, &path)?;
        let meta = std::fs::metadata(&path)?;
        Ok(PutOutcome::Written(ObjectMeta {
            key: key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::surrogate_etag(&meta)),
        }))
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
