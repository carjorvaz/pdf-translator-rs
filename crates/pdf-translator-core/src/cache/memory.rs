use moka::future::Cache;
use std::time::Duration;

/// In-memory cache using moka
pub struct MemoryCache {
    cache: Cache<String, Vec<u8>>,
}

impl MemoryCache {
    pub fn new(max_entries: u64, ttl_seconds: u64) -> Self {
        let mut builder = Cache::builder().max_capacity(max_entries);

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
