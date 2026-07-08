//! In-memory [`ObjectStore`] used for tests and bootstrap wiring.
//!
//! This lets the reconcile/import/export code paths be developed and unit
//! tested without AWS. Swap in the real S3-backed store in production.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::{ObjectMeta, ObjectStore};
use crate::error::{Error, Result};

/// A trivial, process-local object store backed by a `HashMap`.
#[derive(Default)]
pub struct StubStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

impl StubStore {
    pub fn new() -> Self {
        StubStore::default()
    }

    fn etag(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }
}

impl ObjectStore for StubStore {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let objects = self.objects.lock().expect("store lock poisoned");
        objects
            .get(key)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("object {key}")))
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<ObjectMeta> {
        let mut objects = self.objects.lock().expect("store lock poisoned");
        objects.insert(key.to_string(), bytes.to_vec());
        Ok(ObjectMeta {
            key: key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::etag(bytes)),
        })
    }

    fn exists(&self, key: &str) -> Result<bool> {
        let objects = self.objects.lock().expect("store lock poisoned");
        Ok(objects.contains_key(key))
    }

    fn head(&self, key: &str) -> Result<Option<ObjectMeta>> {
        let objects = self.objects.lock().expect("store lock poisoned");
        Ok(objects.get(key).map(|bytes| ObjectMeta {
            key: key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::etag(bytes)),
        }))
    }

    fn copy(&self, from_key: &str, to_key: &str) -> Result<ObjectMeta> {
        let mut objects = self.objects.lock().expect("store lock poisoned");
        let bytes = objects
            .get(from_key)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("object {from_key}")))?;
        objects.insert(to_key.to_string(), bytes.clone());
        Ok(ObjectMeta {
            key: to_key.to_string(),
            size_bytes: bytes.len() as u64,
            etag: Some(Self::etag(&bytes)),
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let objects = self.objects.lock().expect("store lock poisoned");
        let mut keys: Vec<String> = objects
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }
}
