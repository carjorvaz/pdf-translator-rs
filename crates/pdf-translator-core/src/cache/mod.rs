mod memory;
mod disk;
mod key;

pub use memory::MemoryCache;
pub use disk::DiskCache;
pub use key::CacheKey;

use std::sync::Arc;

use crate::config::CacheConfig;
use crate::error::Result;

/// Combined cache with memory and disk layers.
///
/// This is cheaply cloneable via internal `Arc`, allowing a single cache
/// to be shared across multiple `PdfTranslator` instances.
#[derive(Clone)]
pub struct TranslationCache {
    inner: Arc<TranslationCacheInner>,
}

struct TranslationCacheInner {
    memory: Option<MemoryCache>,
    disk: Option<DiskCache>,
}

impl TranslationCache {
    pub fn new(config: &CacheConfig) -> Result<Self> {
        let memory = if config.memory_enabled {
            Some(MemoryCache::new(
                config.memory_max_entries,
                config.memory_ttl_seconds,
            ))
        } else {
            None
        };

        let disk = if config.disk_enabled {
            let path = config.disk_path.clone().unwrap_or_else(|| {
                let cache_dir = crate::util::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from(".cache"));
                cache_dir.join("pdf-translator")
            });
            Some(DiskCache::new(path)?)
        } else {
            None
        };

        Ok(Self {
            inner: Arc::new(TranslationCacheInner { memory, disk }),
        })
    }

    pub async fn get(&self, key: &CacheKey) -> Option<Vec<u8>> {
        let key_str = key.to_string();

        // Try memory cache first
        if let Some(ref memory) = self.inner.memory
            && let Some(value) = memory.get(&key_str).await {
                return Some(value);
            }

        // Try disk cache
        if let Some(ref disk) = self.inner.disk
            && let Some(value) = disk.get(&key_str) {
                // Populate memory cache on disk hit
                if let Some(ref memory) = self.inner.memory {
                    memory.insert(key_str, value.clone()).await;
                }
                return Some(value);
            }

        None
    }

    pub async fn insert(&self, key: &CacheKey, value: Vec<u8>) {
        let key_str = key.to_string();

        // Store in memory cache
        if let Some(ref memory) = self.inner.memory {
            memory.insert(key_str.clone(), value.clone()).await;
        }

        // Store in disk cache
        if let Some(ref disk) = self.inner.disk {
            let _ = disk.insert(&key_str, &value);
        }
    }

    pub async fn contains(&self, key: &CacheKey) -> bool {
        self.get(key).await.is_some()
    }

    pub fn clear(&self) {
        if let Some(ref memory) = self.inner.memory {
            memory.clear();
        }

        if let Some(ref disk) = self.inner.disk {
            let _ = disk.clear();
        }
    }
}
