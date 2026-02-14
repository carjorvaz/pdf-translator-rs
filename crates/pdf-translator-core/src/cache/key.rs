use crate::config::{Lang, TextColor};

/// Cache key for translated PDF pages.
///
/// Keys are opaque MD5 hashes of all relevant inputs, ensuring:
/// - Same document + page + content + settings = same key
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
        source_lang: &Lang,
        target_lang: &Lang,
        text_color: TextColor,
    ) -> Self {
        // Combine all inputs into a single string for hashing.
        // Using null bytes as separators prevents collision between
        // inputs like ("a", "bc") and ("ab", "c").
        let combined = format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{},{},{}",
            doc_id.as_ref(),
            page_num,
            text_content,
            translator.to_lowercase(),
            source_lang.as_str(),
            target_lang.as_str(),
            text_color.r,
            text_color.g,
            text_color.b,
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
        source_lang: &Lang,
        target_lang: &Lang,
        text_color: TextColor,
    ) -> Self {
        Self::new(doc_hash, page_num, page_text, translator, source_lang, target_lang, text_color)
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

    const BLACK: TextColor = TextColor::new(0.0, 0.0, 0.0);

    fn key(doc: &str, page: usize, text: &str, translator: &str, src: &str, tgt: &str) -> CacheKey {
        CacheKey::new(doc, page, text, translator, &Lang::new(src), &Lang::new(tgt), BLACK)
    }

    #[test]
    fn test_cache_key_is_fixed_length_hash() {
        let k = key("doc123", 5, "Hello world", "Google", "fr", "zh-CN");
        assert_eq!(k.to_string().len(), 32);
        assert!(k.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cache_key_differs_by_content() {
        assert_ne!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "World", "Google", "fr", "en"));
    }

    #[test]
    fn test_cache_key_differs_by_page() {
        assert_ne!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 1, "Hello", "Google", "fr", "en"));
    }

    #[test]
    fn test_cache_key_differs_by_translator() {
        assert_ne!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "Hello", "OpenAI", "fr", "en"));
    }

    #[test]
    fn test_cache_key_differs_by_language() {
        assert_ne!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "Hello", "Google", "fr", "zh-CN"));
    }

    #[test]
    fn test_cache_key_differs_by_source_language() {
        assert_ne!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "Hello", "Google", "auto", "en"));
    }

    #[test]
    fn test_cache_key_differs_by_color() {
        let k1 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("fr"), &Lang::new("en"), BLACK);
        let k2 = CacheKey::new("doc", 0, "Hello", "Google", &Lang::new("fr"), &Lang::new("en"), TextColor::new(0.8, 0.0, 0.0));
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_cache_key_same_inputs_same_key() {
        assert_eq!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "Hello", "Google", "fr", "en"));
    }

    #[test]
    fn test_cache_key_case_insensitive_translator() {
        assert_eq!(key("doc", 0, "Hello", "Google", "fr", "en"),
                   key("doc", 0, "Hello", "GOOGLE", "fr", "en"));
    }
}
