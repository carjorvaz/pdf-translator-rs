//! Durable, immutable disk-backed storage for translated PDF pages.

use std::collections::HashMap;
use std::io;
#[cfg(test)]
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracing::debug;
use uuid::Uuid;

pub struct OutputBudget {
    retained: AtomicUsize,
    limit: usize,
}

impl OutputBudget {
    pub(super) const fn new(limit: usize) -> Self {
        Self {
            retained: AtomicUsize::new(0),
            limit,
        }
    }

    pub(super) fn reserve(self: &Arc<Self>, bytes: usize) -> Option<OutputReservation> {
        self.retained
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |retained| {
                retained
                    .checked_add(bytes)
                    .filter(|&total| total <= self.limit)
            })
            .ok()
            .map(|_| OutputReservation {
                budget: Arc::clone(self),
                bytes,
            })
    }
}

pub struct OutputReservation {
    budget: Arc<OutputBudget>,
    bytes: usize,
}

impl Drop for OutputReservation {
    fn drop(&mut self) {
        self.budget.retained.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub struct StagedPage {
    path: Option<PathBuf>,
    reservation: Option<OutputReservation>,
}

impl StagedPage {
    #[allow(clippy::expect_used)] // StagedPage owns its path until publication consumes it.
    fn path(&self) -> &Path {
        self.path.as_deref().expect("staged page path consumed")
    }

    pub(super) fn reserve(&mut self, reservation: OutputReservation) {
        debug_assert!(self.reservation.is_none());
        self.reservation = Some(reservation);
    }
}

impl Drop for StagedPage {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                "Failed to remove staged page at {}: {error}",
                path.display()
            );
            if let Some(reservation) = self.reservation.take() {
                std::mem::forget(reservation);
            }
        }
    }
}

pub struct StoredPage {
    path: PathBuf,
    page: usize,
    version: u64,
    reservation: Option<OutputReservation>,
    _dir: Arc<TempDir>,
}

impl StoredPage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn page(&self) -> usize {
        self.page
    }

    pub const fn version(&self) -> u64 {
        self.version
    }
}

impl Drop for StoredPage {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                "Failed to remove stored page {} v{} at {}: {error}",
                self.page,
                self.version,
                self.path.display()
            );
            if let Some(reservation) = self.reservation.take() {
                std::mem::forget(reservation);
            }
        }
    }
}

pub struct PageStore {
    dir: Arc<TempDir>,
    /// Currently published immutable pages visible through session metadata.
    pages: HashMap<usize, Arc<StoredPage>>,
    /// Monotonic counters retained across metadata clears.
    last_versions: HashMap<usize, u64>,
}

impl PageStore {
    pub fn new() -> io::Result<Self> {
        let dir = TempDir::new()?;
        debug!("Created page store at {}", dir.path().display());
        Ok(Self {
            dir: Arc::new(dir),
            pages: HashMap::new(),
            last_versions: HashMap::new(),
        })
    }

    pub fn page_snapshot(&self, page: usize) -> Option<Arc<StoredPage>> {
        self.pages.get(&page).cloned()
    }

    pub fn all_page_snapshots(&self) -> Vec<Arc<StoredPage>> {
        let mut pages: Vec<_> = self.pages.values().cloned().collect();
        pages.sort_unstable_by_key(|page| page.page());
        pages
    }

    pub fn version(&self, page: usize) -> u64 {
        self.pages.get(&page).map_or(0, |page| page.version())
    }

    pub fn has_page(&self, page: usize) -> bool {
        self.pages.contains_key(&page)
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Allocate a same-directory staging guard. Dropping it removes any
    /// partial or complete staging file.
    pub fn staging_path(&self, page: usize) -> StagedPage {
        StagedPage {
            path: Some(
                self.dir
                    .path()
                    .join(format!(".page_{page}_{}.tmp", Uuid::new_v4())),
            ),
            reservation: None,
        }
    }

    /// Create, fully write, and fsync a staging file without exposing it to readers.
    pub async fn write_staged(staged: &StagedPage, data: &[u8]) -> io::Result<()> {
        if staged.reservation.as_ref().map(|held| held.bytes) != Some(data.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staged page is missing its matching output reservation",
            ));
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staged.path())
            .await?;
        file.write_all(data).await?;
        file.sync_all().await?;
        drop(file);
        Ok(())
    }

    /// Atomically rename and durably publish an already-fsynced staging file.
    ///
    /// The returned immutable handle is not visible to readers until
    /// `mark_published` installs it as the page's current snapshot.
    pub(super) fn publish_staged(
        &mut self,
        page: usize,
        mut staged: StagedPage,
    ) -> io::Result<Arc<StoredPage>> {
        if staged.path().parent() != Some(self.dir.path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staging file is outside page store",
            ));
        }
        let byte_len = usize::try_from(std::fs::metadata(staged.path())?.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "translated page size exceeds platform limits",
            )
        })?;
        if byte_len != staged.reservation.as_ref().map_or(0, |held| held.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged page size does not match its output reservation",
            ));
        }
        let version = self
            .last_versions
            .get(&page)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("page version counter overflow"))?;
        let published = self.versioned_path(page, version);
        if published.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "immutable page version already exists",
            ));
        }

        std::fs::rename(staged.path(), &published)?;
        staged.path = Some(published.clone());
        std::fs::File::open(self.dir.path()).and_then(|dir| dir.sync_all())?;
        self.last_versions.insert(page, version);
        let Some(reservation) = staged.reservation.take() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "validated staged page reservation missing",
            ));
        };
        staged.path = None;
        debug!("Durably published page {page} v{version}");
        Ok(Arc::new(StoredPage {
            path: published,
            page,
            version,
            reservation: Some(reservation),
            _dir: Arc::clone(&self.dir),
        }))
    }

    pub(super) fn mark_published(&mut self, published: Arc<StoredPage>) -> io::Result<()> {
        if self.last_versions.get(&published.page) != Some(&published.version)
            || published.path != self.versioned_path(published.page, published.version)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication does not belong to the current page-store version",
            ));
        }
        self.pages.insert(published.page, published);
        debug!("Marked current translated page active");
        Ok(())
    }

    /// Release the store's ownership of every current page. Reader snapshots
    /// keep their immutable files and directory alive until their final drop.
    pub fn clear(&mut self) {
        self.pages.clear();
        debug!("Cleared active translated-page metadata");
    }

    fn versioned_path(&self, page: usize, version: u64) -> PathBuf {
        self.dir.path().join(format!("page_{page}_v{version}.pdf"))
    }

    #[cfg(test)]
    pub fn store_sync(&mut self, page: usize, data: &[u8]) -> io::Result<()> {
        let mut staged = self.staging_path(page);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staged.path())?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        let budget = Arc::new(OutputBudget::new(usize::MAX));
        let reservation = budget.reserve(data.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::StorageFull,
                "translated output storage limit exceeded",
            )
        })?;
        staged.reserve(reservation);
        let published = self.publish_staged(page, staged)?;
        self.mark_published(published)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn load_sync(&self, page: usize) -> io::Result<Option<Vec<u8>>> {
        self.page_snapshot(page)
            .map(|page| std::fs::read(page.path()))
            .transpose()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stored_versions_are_immutable_and_clear_preserves_snapshots() {
        let mut store = PageStore::new().unwrap();
        store.store_sync(0, b"v1").unwrap();
        let first = store.page_snapshot(0).unwrap();
        store.store_sync(0, b"v2").unwrap();
        let second = store.page_snapshot(0).unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(std::fs::read(first.path()).unwrap(), b"v1");
        assert_eq!(std::fs::read(second.path()).unwrap(), b"v2");

        store.clear();
        assert!(store.is_empty());
        assert_eq!(std::fs::read(first.path()).unwrap(), b"v1");
        assert_eq!(std::fs::read(second.path()).unwrap(), b"v2");

        store.store_sync(0, b"v3").unwrap();
        assert_eq!(store.version(0), 3);
        assert_eq!(
            std::fs::read(store.page_snapshot(0).unwrap().path()).unwrap(),
            b"v3"
        );
        assert_eq!(std::fs::read(first.path()).unwrap(), b"v1");
        assert_eq!(std::fs::read(second.path()).unwrap(), b"v2");
    }

    #[test]
    fn store_and_load() {
        let mut store = PageStore::new().unwrap();
        store.store_sync(0, b"test pdf content").unwrap();
        assert_eq!(store.version(0), 1);
        assert_eq!(store.load_sync(0).unwrap().unwrap(), b"test pdf content");
    }
    #[test]
    fn output_budget_releases_only_when_its_guard_drops() {
        let budget = Arc::new(OutputBudget::new(4));
        let reservation = budget.reserve(3).unwrap();
        assert_eq!(budget.retained.load(Ordering::Acquire), 3);
        assert!(budget.reserve(2).is_none());

        drop(reservation);
        assert_eq!(budget.retained.load(Ordering::Acquire), 0);
        assert!(budget.reserve(4).is_some());
    }

    #[test]
    fn cancelled_staging_removes_file_and_reservation() {
        let store = PageStore::new().unwrap();
        let budget = Arc::new(OutputBudget::new(3));
        let mut staged = store.staging_path(0);
        let path = staged.path().to_path_buf();
        std::fs::write(&path, b"pdf").unwrap();
        staged.reserve(budget.reserve(3).unwrap());

        drop(staged);
        assert!(!path.exists());
        assert_eq!(budget.retained.load(Ordering::Acquire), 0);
    }
}
