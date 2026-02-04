use sled::Db;
use std::path::Path;
use tracing::{debug, warn};

use crate::error::{Error, Result};

/// Disk-based cache using sled
pub struct DiskCache {
    db: Db,
}

impl DiskCache {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::CacheInit(format!(
                    "Failed to create cache directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let db = sled::open(path).map_err(|e| {
            let err_str = e.to_string();
            // Detect lock errors and provide actionable fix
            if err_str.contains("WouldBlock") || err_str.contains("lock") {
                Error::CacheInit(format!(
                    "Cache locked at {}\n\n\
                    Another process is using the cache, or a previous instance crashed.\n\
                    To fix: rm {}/db/LOCK",
                    path.display(),
                    path.display()
                ))
            } else {
                Error::CacheInit(format!("Failed to open cache at {}: {}", path.display(), e))
            }
        })?;

        debug!("Opened disk cache at {}", path.display());

        Ok(Self { db })
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        match self.db.get(key.as_bytes()) {
            Ok(Some(value)) => Some(value.to_vec()),
            Ok(None) => None,
            Err(e) => {
                warn!("Cache read error: {}", e);
                None
            }
        }
    }

    pub fn insert(&self, key: &str, value: &[u8]) -> Result<()> {
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| Error::CacheWrite(e.to_string()))?;

        // Flush to ensure persistence
        self.db
            .flush()
            .map_err(|e| Error::CacheWrite(format!("Flush failed: {e}")))?;

        Ok(())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.db.contains_key(key.as_bytes()).unwrap_or(false)
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        self.db
            .remove(key.as_bytes())
            .map_err(|e| Error::CacheWrite(e.to_string()))?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        self.db.clear().map_err(|e| Error::CacheWrite(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| Error::CacheWrite(format!("Flush failed: {e}")))?;
        Ok(())
    }

    pub fn size_on_disk(&self) -> u64 {
        self.db.size_on_disk().unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.db.len()
    }

    pub fn is_empty(&self) -> bool {
        self.db.is_empty()
    }
}
