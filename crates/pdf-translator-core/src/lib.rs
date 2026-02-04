//! PDF Translator Core Library
//!
//! This library provides the core functionality for translating PDF documents:
//! - PDF text extraction and rendering
//! - Translation via OpenAI-compatible APIs
//! - Caching (memory and disk)
//! - PDF overlay creation for translations

pub mod cache;
pub mod config;
pub mod error;
pub mod pdf;
pub mod translator;
pub mod util;

pub use config::{
    AppConfig, Lang, LanguageOption, TextColor, TranslatorConfig,
    source_languages, target_languages, flag_for_lang,
    DEFAULT_SOURCE_LANG, DEFAULT_TARGET_LANG, DEFAULT_TEXT_COLOR,
};
pub use error::{Error, Result};
pub use pdf::{BoundingBox, PdfDocument, TextBlock, PageRenderer, PdfOverlay, OverlayOptions};
pub use translator::{Translator, OpenAiTranslator, create_translator};
pub use cache::{TranslationCache, CacheKey};
pub use util::clear_translation_cache;

use std::sync::Arc;
use tracing::{debug, info};

/// High-level PDF translator that combines all components
pub struct PdfTranslator {
    translator: Arc<dyn Translator>,
    cache: TranslationCache,
    config: AppConfig,
}

/// Result of translating a single page
pub struct TranslatedPage {
    /// Page number (0-indexed)
    pub page_num: usize,
    /// Translated PDF bytes (single page)
    pub pdf_bytes: Vec<u8>,
    /// Whether this was a cache hit
    pub from_cache: bool,
}

impl PdfTranslator {
    /// Create a new PDF translator with the given configuration
    pub fn new(config: AppConfig) -> Result<Self> {
        let translator = create_translator(&config.translator)?;
        let cache = TranslationCache::new(&config.cache)?;

        Ok(Self {
            translator,
            cache,
            config,
        })
    }

    /// Create with a shared cache (for cache sharing across instances)
    pub fn with_cache(config: AppConfig, cache: TranslationCache) -> Result<Self> {
        let translator = create_translator(&config.translator)?;

        Ok(Self {
            translator,
            cache,
            config,
        })
    }

    /// Create with a custom translator
    pub fn with_translator(
        translator: Arc<dyn Translator>,
        config: AppConfig,
    ) -> Result<Self> {
        let cache = TranslationCache::new(&config.cache)?;

        Ok(Self {
            translator,
            cache,
            config,
        })
    }

    /// Translate a single page of a PDF document
    pub async fn translate_page(
        &self,
        doc: &PdfDocument,
        page_num: usize,
    ) -> Result<TranslatedPage> {
        self.translate_page_impl(doc, page_num, false, None).await
    }

    /// Translate a single page, optionally bypassing cache
    pub async fn translate_page_force(
        &self,
        doc: &PdfDocument,
        page_num: usize,
        force: bool,
    ) -> Result<TranslatedPage> {
        self.translate_page_impl(doc, page_num, force, None).await
    }

    /// Translate a page as a prefetch (logged differently)
    pub async fn translate_page_prefetch(
        &self,
        doc: &PdfDocument,
        page_num: usize,
    ) -> Result<TranslatedPage> {
        self.translate_page_impl(doc, page_num, false, Some("(prefetch)")).await
    }

    /// Internal implementation of translate_page
    async fn translate_page_impl(
        &self,
        doc: &PdfDocument,
        page_num: usize,
        force: bool,
        label: Option<&str>,
    ) -> Result<TranslatedPage> {
        // Extract text for cache key
        let extractor = pdf::TextExtractor::new(doc);
        let page_text = extractor.get_page_text(page_num)?;

        // Generate cache key
        let cache_key = CacheKey::from_page(
            &doc.cache_id(),
            page_num,
            &page_text,
            self.translator.name(),
            &self.config.target_lang,
        );

        // Check cache (unless force is set)
        if !force
            && let Some(cached) = self.cache.get(&cache_key).await
        {
            debug!("Cache hit for page {}", page_num);
            return Ok(TranslatedPage {
                page_num,
                pdf_bytes: cached,
                from_cache: true,
            });
        }

        info!(
            "Translating page {} with {}{}{}",
            page_num,
            self.translator.name(),
            if force { " (forced)" } else { "" },
            label.unwrap_or("")
        );

        // Extract text blocks
        let blocks = extractor.extract_page_blocks(page_num)?;

        // Translate each block
        let mut overlays = Vec::with_capacity(blocks.len());
        for block in blocks {
            if block.text.trim().is_empty() {
                continue;
            }

            let translated = self
                .translator
                .translate(&block.text, &self.config.source_lang, &self.config.target_lang)
                .await?;

            overlays.push(pdf::overlay::TranslationOverlay {
                bbox: block.bbox,
                original: block.text,
                translated,
                font_size: block.font_size,
            });
        }

        // Create PDF overlay
        let overlay_options = OverlayOptions {
            text_color: self.config.text_color,
            ..Default::default()
        };

        let overlay = PdfOverlay::new(overlay_options)?;
        let pdf_bytes = overlay.create_translated_page(doc.bytes(), page_num, &overlays)?;

        // Store in cache
        self.cache.insert(&cache_key, pdf_bytes.clone()).await;

        Ok(TranslatedPage {
            page_num,
            pdf_bytes,
            from_cache: false,
        })
    }

    /// Translate all pages and combine into a single PDF
    pub async fn translate_document(
        &self,
        doc: &PdfDocument,
        progress_callback: Option<Box<dyn Fn(usize, usize) + Send>>,
    ) -> Result<Vec<u8>> {
        let total_pages = doc.page_count();
        let mut translated_pages = Vec::with_capacity(total_pages);

        for page_num in 0..total_pages {
            let result = self.translate_page(doc, page_num).await?;
            translated_pages.push(result.pdf_bytes);

            if let Some(ref callback) = progress_callback {
                callback(page_num + 1, total_pages);
            }
        }

        // Combine all pages
        pdf::overlay::combine_pdfs(&translated_pages)
    }

    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn translator_info(&self) -> translator::TranslatorInfo {
        self.translator.info()
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
    }
}

/// Convenience function to render a page from a document as PNG
pub fn render_page(
    doc: &PdfDocument,
    page_num: usize,
    scale: f32,
) -> Result<Vec<u8>> {
    let renderer = PageRenderer::with_scale(doc, scale);
    renderer.render_page_png(page_num)
}

/// Convenience function to render a page from a document as WebP (lossless)
pub fn render_page_webp(
    doc: &PdfDocument,
    page_num: usize,
    scale: f32,
) -> Result<Vec<u8>> {
    let renderer = PageRenderer::with_scale(doc, scale);
    renderer.render_page_webp(page_num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.source_lang.as_str(), "fr");
        assert_eq!(config.target_lang.as_str(), "en");
    }
}
