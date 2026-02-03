//! Disk-backed storage for translated PDF pages.
//!
//! Instead of keeping all translated PDFs in memory, we write them to
//! temporary files and serve them lazily. This reduces memory usage while
//! letting HTTP caching handle the "hot path" at the browser level.
//!
//! ## Design: Separating Metadata from I/O
//!
//! The PageStore separates fast metadata operations (version tracking, path
//! generation) from slow I/O operations (reading/writing files). This allows:
//!
//! - Metadata ops inside session locks (fast, won't block other requests)
//! - File I/O outside locks with `tokio::fs` (async, won't block runtime)
//!
//! Each session gets its own temp directory that's automatically cleaned
//! up when the PageStore (and thus the Session) is dropped.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use tempfile::TempDir;
use tracing::debug;

/// Disk-backed storage for translated pages with version tracking.
///
/// Pages are stored as individual PDF files in a temp directory.
/// Version numbers enable proper HTTP caching with ETags.
///
/// # Usage Pattern
///
/// ```ignore
/// // Inside session lock - fast metadata only
/// let path = session.page_store.page_path(page);
/// let version = session.page_store.version(page);
///
/// // Outside session lock - async I/O
/// let data = tokio::fs::read(&path).await?;
/// ```
pub struct PageStore {
    /// Temp directory - auto-cleaned on drop
    dir: TempDir,
    /// Version counter per page (for ETag generation)
    versions: HashMap<usize, u64>,
}

impl PageStore {
    /// Create a new page store with a fresh temp directory.
    pub fn new() -> io::Result<Self> {
        let dir = TempDir::new()?;
        debug!("Created page store at {}", dir.path().display());
        Ok(Self {
            dir,
            versions: HashMap::new(),
        })
    }

    // =========================================================================
    // Metadata operations (fast, safe inside session locks)
    // =========================================================================

    /// Get the file path for a page.
    ///
    /// This is a fast operation - just string concatenation.
    /// Use this inside session locks, then do I/O outside.
    pub fn page_path(&self, page: usize) -> PathBuf {
        self.dir.path().join(format!("page_{page}.pdf"))
    }

    /// Get paths for all translated pages, in order.
    ///
    /// Use this inside session locks to get paths, then load outside.
    pub fn all_page_paths(&self) -> Vec<PathBuf> {
        let mut pages: Vec<_> = self.versions.keys().copied().collect();
        pages.sort_unstable();
        pages.into_iter().map(|p| self.page_path(p)).collect()
    }

    /// Get the version number for a page (0 if not translated).
    pub fn version(&self, page: usize) -> u64 {
        self.versions.get(&page).copied().unwrap_or(0)
    }

    pub fn has_page(&self, page: usize) -> bool {
        self.versions.contains_key(&page)
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// Register that a page has been stored (updates version).
    ///
    /// Call this AFTER successfully writing the file.
    /// This is fast - just updates the in-memory version counter.
    pub fn mark_stored(&mut self, page: usize) {
        let version = self.versions.entry(page).or_insert(0);
        *version += 1;
        debug!("Marked page {} stored, v{}", page, version);
    }

    /// Clear all translated pages (sync).
    ///
    /// Removes all page files and resets version counters.
    pub fn clear(&mut self) {
        for page in self.versions.keys() {
            let path = self.page_path(*page);
            let _ = std::fs::remove_file(path);
        }
        self.versions.clear();
        debug!("Cleared all translated pages");
    }

    // =========================================================================
    // Test helpers - sync I/O for unit tests
    // =========================================================================

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// Store a translated page to disk (sync, for tests).
    #[cfg(test)]
    pub fn store_sync(&mut self, page: usize, data: &[u8]) -> io::Result<()> {
        let path = self.page_path(page);
        std::fs::write(&path, data)?;
        self.mark_stored(page);
        debug!("Stored page {} ({} bytes)", page, data.len());
        Ok(())
    }

    /// Load a translated page from disk (sync, for tests).
    #[cfg(test)]
    pub fn load_sync(&self, page: usize) -> io::Result<Option<Vec<u8>>> {
        if !self.has_page(page) {
            return Ok(None);
        }
        let path = self.page_path(page);
        Ok(Some(std::fs::read(&path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_load() {
        let mut store = PageStore::new().unwrap();
        let data = b"test pdf content";

        store.store_sync(0, data).unwrap();
        assert!(store.has_page(0));
        assert!(!store.has_page(1));

        let loaded = store.load_sync(0).unwrap().unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_versioning() {
        let mut store = PageStore::new().unwrap();

        assert_eq!(store.version(0), 0);

        store.store_sync(0, b"v1").unwrap();
        assert_eq!(store.version(0), 1);

        store.store_sync(0, b"v2").unwrap();
        assert_eq!(store.version(0), 2);
    }

    #[test]
    fn test_clear() {
        let mut store = PageStore::new().unwrap();

        store.store_sync(0, b"page 0").unwrap();
        store.store_sync(1, b"page 1").unwrap();
        assert_eq!(store.len(), 2);

        store.clear();
        assert!(store.is_empty());
        assert!(!store.has_page(0));
    }

    #[test]
    fn test_metadata_only() {
        let mut store = PageStore::new().unwrap();

        // Get path without storing
        let path = store.page_path(5);
        assert!(path.to_string_lossy().contains("page_5.pdf"));

        // Mark stored updates version
        store.mark_stored(5);
        assert_eq!(store.version(5), 1);
        assert!(store.has_page(5));
    }
}
