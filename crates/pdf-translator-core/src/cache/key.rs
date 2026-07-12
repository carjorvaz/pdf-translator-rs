use crate::config::{Lang, TextColor, TranslatorCacheIdentity};

const _: () = assert!(usize::BITS <= u64::BITS);

#[allow(clippy::cast_possible_truncation)]
const fn usize_as_u64(value: usize) -> u64 {
    value as u64
}

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
        translator: &TranslatorCacheIdentity,
        source_lang: &Lang,
        target_lang: &Lang,
        text_color: TextColor,
    ) -> Self {
        fn consume_field(context: &mut md5::Context, value: &[u8]) {
            context.consume(usize_as_u64(value.len()).to_be_bytes());
            context.consume(value);
        }

        let mut context = md5::Context::new();
        consume_field(&mut context, b"pdf-translator-cache-key-v2");
        consume_field(&mut context, doc_id.as_ref().as_bytes());
        context.consume(usize_as_u64(page_num).to_be_bytes());
        consume_field(&mut context, text_content.as_bytes());
        consume_field(&mut context, translator.backend().as_bytes());
        consume_field(&mut context, translator.endpoint().as_bytes());
        consume_field(&mut context, translator.model().as_bytes());
        consume_field(&mut context, source_lang.as_str().as_bytes());
        consume_field(&mut context, target_lang.as_str().as_bytes());
        context.consume(text_color.r.to_bits().to_be_bytes());
        context.consume(text_color.g.to_bits().to_be_bytes());
        context.consume(text_color.b.to_bits().to_be_bytes());

        Self {
            hash: format!("{:x}", context.compute()),
        }
    }

    pub fn from_page(
        doc_hash: &str,
        page_num: usize,
        page_text: &str,
        translator: &TranslatorCacheIdentity,
        source_lang: &Lang,
        target_lang: &Lang,
        text_color: TextColor,
    ) -> Self {
        Self::new(
            doc_hash,
            page_num,
            page_text,
            translator,
            source_lang,
            target_lang,
            text_color,
        )
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
        let identity = TranslatorCacheIdentity::new(translator, "https://example.test/v1", "model");
        CacheKey::new(
            doc,
            page,
            text,
            &identity,
            &Lang::new(src),
            &Lang::new(tgt),
            BLACK,
        )
    }

    #[test]
    fn test_cache_key_is_fixed_length_hash() {
        let k = key("doc123", 5, "Hello world", "Google", "fr", "zh-CN");
        assert_eq!(k.to_string().len(), 32);
        assert!(k.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cache_key_differs_by_content() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "World", "Google", "fr", "en")
        );
    }

    #[test]
    fn test_cache_key_differs_by_page() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 1, "Hello", "Google", "fr", "en")
        );
    }

    #[test]
    fn test_cache_key_differs_by_translator() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "Hello", "OpenAI", "fr", "en")
        );
    }

    #[test]
    fn test_cache_key_differs_by_language() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "Hello", "Google", "fr", "zh-CN")
        );
    }

    #[test]
    fn test_cache_key_differs_by_source_language() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "Hello", "Google", "auto", "en")
        );
    }

    #[test]
    fn test_cache_key_differs_by_color() {
        let identity = TranslatorCacheIdentity::new("OpenAI", "https://example.test/v1", "model");
        let k1 = CacheKey::new(
            "doc",
            0,
            "Hello",
            &identity,
            &Lang::new("fr"),
            &Lang::new("en"),
            BLACK,
        );
        let k2 = CacheKey::new(
            "doc",
            0,
            "Hello",
            &identity,
            &Lang::new("fr"),
            &Lang::new("en"),
            TextColor::new(0.8, 0.0, 0.0),
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_cache_key_same_inputs_same_key() {
        assert_eq!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "Hello", "Google", "fr", "en")
        );
    }

    #[test]
    fn test_cache_key_distinguishes_translator_identity_case() {
        assert_ne!(
            key("doc", 0, "Hello", "Google", "fr", "en"),
            key("doc", 0, "Hello", "GOOGLE", "fr", "en")
        );
    }
}
