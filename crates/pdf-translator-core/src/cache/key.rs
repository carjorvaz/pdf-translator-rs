use crate::config::Lang;

/// Cache key for translated PDF pages.
///
/// Keys are opaque MD5 hashes of all relevant inputs, ensuring:
/// - Same document + page + content + translator + language = same key
/// - Any change to inputs produces a different key
/// - Keys are fixed-length (32 hex chars) for consistent storage
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    hash: String,
}

impl CacheKey {
    pub fn new(
        doc_id: impl AsRef<str>,
        page_num: usize,
        text_content: &str,
        translator: &str,
        target_lang: &Lang,
    ) -> Self {
        // Combine all inputs into a single string for hashing.
        // Using null bytes as separators prevents collision between
        // inputs like ("a", "bc") and ("ab", "c").
        let combined = format!(
            "{}\0{}\0{}\0{}\0{}",
            doc_id.as_ref(),
            page_num,
            text_content,
            translator.to_lowercase(),
            target_lang.as_str()
        );

        Self {
            hash: format!("{:x}", md5::compute(combined.as_bytes())),
        }
    }

    pub fn from_page(
        doc_hash: &str,
        page_num: usize,
        page_text: &str,
        translator: &str,
        target_lang: &Lang,
    ) -> Self {
        Self::new(doc_hash, page_num, page_text, translator, target_lang)
    }

    pub fn as_str(&self) -> &str {
        &self.hash
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Just output the hash - no need for human-readable format
        // since cache keys are opaque identifiers
        write!(f, "{}", self.hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_is_fixed_length_hash() {
        let key = CacheKey::new(
            "doc123",
            5,
            "Hello world",
            "Google",
            &Lang::new("zh-CN"),
        );

        // MD5 produces 32 hex characters
        assert_eq!(key.to_string().len(), 32);
        assert!(key.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cache_key_differs_by_content() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 0, "World", "Google", &Lang::new("en"));

        assert_ne!(key1, key2);
        assert_ne!(key1.to_string(), key2.to_string());
    }

    #[test]
    fn test_cache_key_differs_by_page() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 1, "Hello", "Google", &Lang::new("en"));

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_cache_key_differs_by_translator() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 0, "Hello", "OpenAI", &Lang::new("en"));

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_cache_key_differs_by_language() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("zh-CN"));

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_cache_key_same_inputs_same_key() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));

        assert_eq!(key1, key2);
        assert_eq!(key1.to_string(), key2.to_string());
    }

    #[test]
    fn test_cache_key_case_insensitive_translator() {
        let key1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("en"));
        let key2 = CacheKey::new("doc", 0, "Hello", "GOOGLE", &Lang::new("en"));

        assert_eq!(key1, key2);
    }
}
