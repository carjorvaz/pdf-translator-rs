use moka::future::Cache;
use std::time::Duration;

/// In-memory cache using moka with byte-size-based eviction.
pub struct MemoryCache {
    cache: Cache<String, Vec<u8>>,
}

impl MemoryCache {
    pub fn new(max_mb: u64, ttl_seconds: u64) -> Self {
        let max_bytes = max_mb.saturating_mul(1024 * 1024);

        let mut builder = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_key: &String, value: &Vec<u8>| -> u32 {
                // Weight is the value byte size, capped at u32::MAX
                value.len().try_into().unwrap_or(u32::MAX)
            });

        if ttl_seconds > 0 {
            builder = builder.time_to_live(Duration::from_secs(ttl_seconds));
        }

        Self {
            cache: builder.build(),
        }
    }

    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.cache.get(key).await
    }

    pub async fn insert(&self, key: String, value: Vec<u8>) {
        self.cache.insert(key, value).await;
    }

    pub async fn remove(&self, key: &str) {
        self.cache.remove(key).await;
    }

    pub fn clear(&self) {
        self.cache.invalidate_all();
    }
}
