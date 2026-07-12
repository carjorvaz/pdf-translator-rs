use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tracing::{debug, warn};

use crate::error::{Error, Result};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);
const CACHE_KEY_LEN: usize = 32;
const TEMP_PREFIX: &str = ".tmp-";

/// Disk-based cache using one atomically replaced file per opaque cache key.
#[derive(Clone)]
pub struct DiskCache {
    inner: Arc<DiskCacheInner>,
}

struct DiskCacheInner {
    path: PathBuf,
    operations: RwLock<()>,
}

impl DiskCache {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        fs::create_dir_all(path).map_err(|error| {
            Error::CacheInit(format!(
                "Failed to create cache directory {}: {error}",
                path.display()
            ))
        })?;

        debug!("Opened disk cache at {}", path.display());

        Ok(Self {
            inner: Arc::new(DiskCacheInner {
                path: path.to_path_buf(),
                operations: RwLock::new(()),
            }),
        })
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.key_path(key)?;
        let _guard = self
            .inner
            .operations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => match fs::read(&path) {
                Ok(value) => Some(value),
                Err(error) => {
                    warn!("Cache read error for {}: {error}", path.display());
                    None
                }
            },
            Ok(_) => None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                warn!("Cache metadata error for {}: {error}", path.display());
                None
            }
        }
    }

    pub fn insert(&self, key: &str, value: &[u8]) -> Result<()> {
        let destination = self.validated_key_path(key)?;
        let _guard = self
            .inner
            .operations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (temporary, mut file) = self.create_temporary_file()?;

        let write_result = (|| -> io::Result<()> {
            file.write_all(value)?;
            file.sync_all()?;
            drop(file);

            match fs::rename(&temporary, &destination) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                    ) && destination.exists() =>
                {
                    fs::remove_file(&destination)?;
                    fs::rename(&temporary, &destination)?;
                }
                Err(error) => return Err(error),
            }
            sync_directory(&self.inner.path)
        })();

        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(Error::CacheWrite(format!(
                "Failed to atomically write {}: {error}",
                destination.display()
            )));
        }

        Ok(())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        let path = self.validated_key_path(key)?;
        let _guard = self
            .inner
            .operations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.inner.path)
                .map_err(|error| Error::CacheWrite(format!("Failed to persist removal: {error}"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::CacheWrite(format!(
                "Failed to remove {}: {error}",
                path.display()
            ))),
        }
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self
            .inner
            .operations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = fs::read_dir(&self.inner.path)
            .map_err(|error| Error::CacheWrite(format!("Failed to read cache: {error}")))?;
        let mut removed_any = false;

        for entry in entries {
            let entry = entry
                .map_err(|error| Error::CacheWrite(format!("Failed to read cache: {error}")))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (is_valid_key(&name) || name.starts_with(TEMP_PREFIX))
                && entry
                    .file_type()
                    .map_err(|error| Error::CacheWrite(error.to_string()))?
                    .is_file()
            {
                fs::remove_file(entry.path()).map_err(|error| {
                    Error::CacheWrite(format!(
                        "Failed to remove cache entry {}: {error}",
                        entry.path().display()
                    ))
                })?;
                removed_any = true;
            }
        }

        if removed_any {
            sync_directory(&self.inner.path)
                .map_err(|error| Error::CacheWrite(format!("Failed to persist clear: {error}")))?;
        }
        Ok(())
    }

    pub fn size_on_disk(&self) -> u64 {
        self.cache_files()
            .map(|files| {
                files
                    .filter_map(|entry| entry.metadata().ok().map(|m| m.len()))
                    .sum()
            })
            .unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.cache_files().map(Iterator::count).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn key_path(&self, key: &str) -> Option<PathBuf> {
        is_valid_key(key).then(|| self.inner.path.join(key))
    }

    fn validated_key_path(&self, key: &str) -> Result<PathBuf> {
        self.key_path(key).ok_or_else(|| {
            Error::CacheWrite(
                "Cache key must be exactly 32 lowercase hexadecimal characters".into(),
            )
        })
    }

    fn create_temporary_file(&self) -> Result<(PathBuf, File)> {
        for _ in 0..16 {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let name = format!("{TEMP_PREFIX}{}-{sequence:016x}", std::process::id());
            let path = self.inner.path.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(Error::CacheWrite(format!(
                        "Failed to create temporary cache file: {error}"
                    )));
                }
            }
        }

        Err(Error::CacheWrite(
            "Failed to allocate a unique temporary cache file".into(),
        ))
    }

    fn cache_files(&self) -> io::Result<impl Iterator<Item = fs::DirEntry>> {
        let _guard = self
            .inner
            .operations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = fs::read_dir(&self.inner.path)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                is_valid_key(&entry.file_name().to_string_lossy())
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
            })
            .collect::<Vec<_>>();
        Ok(entries.into_iter())
    }
}

fn is_valid_key(key: &str) -> bool {
    key.len() == CACHE_KEY_LEN
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_KEY: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn entries_persist_and_can_be_replaced_and_removed() {
        let directory = tempfile::tempdir().expect("temp directory");
        let cache = DiskCache::new(directory.path()).expect("cache");

        cache.insert(KEY, b"first").expect("initial insert");
        cache.insert(KEY, b"second").expect("replacement insert");
        assert_eq!(cache.get(KEY).as_deref(), Some(b"second".as_slice()));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.size_on_disk(), 6);

        drop(cache);
        let reopened = DiskCache::new(directory.path()).expect("reopen cache");
        assert_eq!(reopened.get(KEY).as_deref(), Some(b"second".as_slice()));
        reopened.remove(KEY).expect("remove");
        reopened.remove(KEY).expect("idempotent remove");
        assert!(reopened.is_empty());
    }

    #[test]
    fn incomplete_temporary_files_are_never_cache_hits() {
        let directory = tempfile::tempdir().expect("temp directory");
        let cache = DiskCache::new(directory.path()).expect("cache");
        fs::write(directory.path().join(".tmp-interrupted"), b"partial").expect("partial write");

        assert_eq!(cache.get(KEY), None);
        assert!(!cache.contains(KEY));
        assert_eq!(cache.len(), 0);

        cache.clear().expect("clear temporary files");
        assert!(!directory.path().join(".tmp-interrupted").exists());
    }

    #[test]
    fn independent_handles_publish_only_complete_values() {
        let directory = tempfile::tempdir().expect("temp directory");
        let first = DiskCache::new(directory.path()).expect("first handle");
        let second = DiskCache::new(directory.path()).expect("second handle");
        let first_value = vec![b'a'; 16 * 1024];
        let second_value = vec![b'b'; 16 * 1024];

        let first_writer = {
            let value = first_value.clone();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    first.insert(KEY, &value).expect("first writer");
                }
            })
        };
        let second_writer = {
            let value = second_value.clone();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    second.insert(KEY, &value).expect("second writer");
                }
            })
        };
        first_writer.join().expect("first thread");
        second_writer.join().expect("second thread");

        let reopened = DiskCache::new(directory.path()).expect("reopened handle");
        let stored = reopened.get(KEY).expect("published value");
        assert!(stored == first_value || stored == second_value);
    }

    #[test]
    fn invalid_keys_cannot_address_paths_outside_cache() {
        let directory = tempfile::tempdir().expect("temp directory");
        let cache = DiskCache::new(directory.path().join("cache")).expect("cache");
        let outside = directory.path().join("outside");
        fs::write(&outside, b"keep").expect("outside file");

        assert!(cache.insert("../outside", b"overwrite").is_err());
        assert_eq!(cache.get("../outside"), None);
        assert!(cache.remove("../outside").is_err());
        assert_eq!(fs::read(outside).expect("outside contents"), b"keep");
    }

    #[test]
    fn clear_removes_entries_but_preserves_unowned_files() {
        let directory = tempfile::tempdir().expect("temp directory");
        let cache = DiskCache::new(directory.path()).expect("cache");
        cache.insert(KEY, b"one").expect("first insert");
        cache.insert(OTHER_KEY, b"two").expect("second insert");
        fs::write(directory.path().join("README"), b"unowned").expect("unowned file");

        cache.clear().expect("clear");

        assert!(cache.is_empty());
        assert_eq!(
            fs::read(directory.path().join("README")).expect("unowned contents"),
            b"unowned"
        );
    }
}
