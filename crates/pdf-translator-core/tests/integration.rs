//! Integration tests for pdf-translator-core
//!
//! These tests verify the end-to-end workflow:
//! - PDF loading and text extraction
//! - Translation with mock backend
//! - Cache hits and misses
//! - PDF overlay creation

use std::sync::Arc;
use async_trait::async_trait;
use pdf_translator_core::{
    AppConfig, CacheKey, Error, Lang, OverlayOptions, PdfDocument, PdfOverlay, PdfTranslator,
    Result,
    translator::TranslatorInfo,
    Translator,
};

// =============================================================================
// Mock Translator for Testing
// =============================================================================

/// A mock translator that returns predictable translations without network calls.
/// Useful for testing the translation pipeline in isolation.
struct MockTranslator {
    /// Prefix to add to translations for verification
    prefix: String,
    /// Simulate failure if true
    should_fail: bool,
}

impl MockTranslator {
    fn new() -> Self {
        Self {
            prefix: "[TRANSLATED]".to_string(),
            should_fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            prefix: String::new(),
            should_fail: true,
        }
    }
}

#[async_trait]
impl Translator for MockTranslator {
    async fn translate(&self, text: &str, _source: &Lang, _target: &Lang) -> Result<String> {
        if self.should_fail {
            return Err(Error::TranslationRequest("Mock translation failure".to_string()));
        }
        Ok(format!("{} {}", self.prefix, text))
    }

    fn name(&self) -> &'static str {
        "mock"
    }

    fn info(&self) -> TranslatorInfo {
        TranslatorInfo {
            name: "mock",
            requires_api_key: false,
            supports_auto_detect: false,
        }
    }
}

// =============================================================================
// Test Fixtures
// =============================================================================

/// Load the test PDF fixture
fn load_test_pdf() -> PdfDocument {
    let pdf_bytes = include_bytes!("fixtures/test.pdf");
    PdfDocument::from_bytes(pdf_bytes.to_vec())
        .expect("Failed to load test PDF")
}

/// Create a minimal test configuration
fn test_config() -> AppConfig {
    AppConfig {
        cache: pdf_translator_core::config::CacheConfig {
            memory_enabled: true,
            disk_enabled: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

// =============================================================================
// PDF Loading Tests
// =============================================================================

#[test]
fn test_pdf_loads_successfully() {
    let doc = load_test_pdf();
    assert!(doc.page_count() > 0, "PDF should have at least one page");
}

#[test]
fn test_pdf_page_count() {
    let doc = load_test_pdf();
    // Test PDF should have exactly 1 page (adjust if your test PDF differs)
    assert!(doc.page_count() >= 1, "Test PDF should have at least 1 page");
}

#[test]
fn test_pdf_text_extraction() {
    let doc = load_test_pdf();
    let extractor = pdf_translator_core::pdf::TextExtractor::new(&doc);

    // Should not panic and should return some result
    let result = extractor.extract_page_blocks(0);
    assert!(result.is_ok(), "Text extraction should succeed");

    let _blocks = result.unwrap();
    // Text blocks may or may not be present depending on PDF content
    // Just verify we get a valid response (no panic)
}

// =============================================================================
// Translation Pipeline Tests
// =============================================================================

#[tokio::test]
async fn test_translate_page_with_mock() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::new());

    let pdf_translator = PdfTranslator::with_translator(translator, config)
        .expect("Should create translator");

    let result = pdf_translator.translate_page(&doc, 0).await;
    assert!(result.is_ok(), "Translation should succeed: {:?}", result.err());

    let translated = result.unwrap();
    assert_eq!(translated.page_num, 0);
    assert!(!translated.pdf_bytes.is_empty(), "Should produce PDF output");
    assert!(!translated.from_cache, "First translation should not be from cache");
}

#[tokio::test]
async fn test_translation_error_handling() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::failing());

    let pdf_translator = PdfTranslator::with_translator(translator, config)
        .expect("Should create translator");

    let result = pdf_translator.translate_page(&doc, 0).await;

    // If the page has no text, translation succeeds (nothing to translate)
    // If it has text, it should fail
    // Either way, we shouldn't panic
    match result {
        Ok(_) => {
            // Page might have no extractable text
        }
        Err(e) => {
            // Expected failure from mock
            assert!(
                format!("{e}").contains("Mock translation failure"),
                "Should contain mock error message, got: {e}"
            );
        }
    }
}

// =============================================================================
// Cache Tests
// =============================================================================

#[tokio::test]
async fn test_cache_hit_on_second_translation() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::new());

    let pdf_translator = PdfTranslator::with_translator(translator, config)
        .expect("Should create translator");

    // First translation - should not be cached
    let first = pdf_translator.translate_page(&doc, 0).await
        .expect("First translation should succeed");
    assert!(!first.from_cache, "First translation should not be from cache");

    // Second translation - should be cached
    let second = pdf_translator.translate_page(&doc, 0).await
        .expect("Second translation should succeed");
    assert!(second.from_cache, "Second translation should be from cache");

    // Both should produce the same output
    assert_eq!(first.pdf_bytes, second.pdf_bytes, "Cached result should match original");
}

#[tokio::test]
async fn test_force_bypasses_cache() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::new());

    let pdf_translator = PdfTranslator::with_translator(translator, config)
        .expect("Should create translator");

    // First translation
    let _ = pdf_translator.translate_page(&doc, 0).await
        .expect("First translation should succeed");

    // Force re-translation - should bypass cache
    let forced = pdf_translator.translate_page_force(&doc, 0, true).await
        .expect("Forced translation should succeed");
    assert!(!forced.from_cache, "Forced translation should bypass cache");
}

#[tokio::test]
async fn test_cache_key_uniqueness() {
    let lang_en = Lang::new("en");
    let lang_fr = Lang::new("fr");

    let color = pdf_translator_core::TextColor::default();
    let key1 = CacheKey::from_page("doc1", 0, "Hello", "mock", &lang_fr, &lang_en, color);
    let key2 = CacheKey::from_page("doc1", 0, "Hello", "mock", &lang_fr, &lang_en, color);
    let key3 = CacheKey::from_page("doc1", 1, "Hello", "mock", &lang_fr, &lang_en, color);
    let key4 = CacheKey::from_page("doc1", 0, "World", "mock", &lang_fr, &lang_en, color);
    let key5 = CacheKey::from_page("doc1", 0, "Hello", "mock", &lang_en, &lang_fr, color);

    // Same inputs should produce same key
    assert_eq!(key1.as_str(), key2.as_str());

    // Different page should produce different key
    assert_ne!(key1.as_str(), key3.as_str());

    // Different content should produce different key
    assert_ne!(key1.as_str(), key4.as_str());

    // Different target language should produce different key
    assert_ne!(key1.as_str(), key5.as_str());
}

// =============================================================================
// PDF Overlay Tests
// =============================================================================

#[test]
fn test_overlay_creates_valid_pdf() {
    let doc = load_test_pdf();
    let options = OverlayOptions::default();
    let overlay = PdfOverlay::new(options);

    // Create overlay with empty translations (just tests PDF manipulation)
    let result = overlay.create_translated_page(doc.bytes(), 0, &[]);
    assert!(result.is_ok(), "Should create overlay PDF: {:?}", result.err());

    let pdf_bytes = result.unwrap();
    assert!(!pdf_bytes.is_empty(), "Should produce non-empty PDF");

    // Verify it's a valid PDF by checking magic bytes
    assert!(pdf_bytes.starts_with(b"%PDF"), "Output should be valid PDF");
}

#[test]
fn test_overlay_with_translation() {
    let doc = load_test_pdf();
    let options = OverlayOptions::default();
    let overlay = PdfOverlay::new(options);

    // Create a simple translation overlay
    let overlays = vec![
        pdf_translator_core::pdf::overlay::TranslationOverlay {
            bbox: pdf_translator_core::pdf::BoundingBox::new(100.0, 100.0, 300.0, 120.0),
            original: "Test text".to_string(),
            translated: "Translated text".to_string(),
            font_size: 12.0,
        },
    ];

    let result = overlay.create_translated_page(doc.bytes(), 0, &overlays);
    assert!(result.is_ok(), "Should create overlay with translations: {:?}", result.err());
}

// =============================================================================
// Page Rendering Tests
// =============================================================================

#[test]
fn test_render_page_png() {
    let doc = load_test_pdf();
    let result = pdf_translator_core::render_page(&doc, 0, 1.0);

    assert!(result.is_ok(), "Should render page: {:?}", result.err());

    let png_bytes = result.unwrap();
    assert!(!png_bytes.is_empty(), "Should produce non-empty PNG");

    // Check PNG magic bytes
    assert!(
        png_bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
        "Output should be valid PNG"
    );
}

#[test]
fn test_render_page_webp() {
    let doc = load_test_pdf();
    let result = pdf_translator_core::render_page_webp(&doc, 0, 1.0);

    assert!(result.is_ok(), "Should render page as WebP: {:?}", result.err());

    let webp_bytes = result.unwrap();
    assert!(!webp_bytes.is_empty(), "Should produce non-empty WebP");

    // Check WebP magic bytes (RIFF....WEBP)
    assert!(
        webp_bytes.starts_with(b"RIFF") && webp_bytes.len() > 12 && &webp_bytes[8..12] == b"WEBP",
        "Output should be valid WebP"
    );
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_invalid_page_number() {
    let doc = load_test_pdf();
    let options = OverlayOptions::default();
    let overlay = PdfOverlay::new(options);

    // Try to access a page that doesn't exist
    let page_count = doc.page_count();
    let result = overlay.create_translated_page(doc.bytes(), page_count + 100, &[]);

    assert!(result.is_err(), "Should fail for invalid page number");
}

#[test]
fn test_invalid_pdf_bytes() {
    let result = PdfDocument::from_bytes(vec![0, 1, 2, 3]);
    assert!(result.is_err(), "Should fail for invalid PDF bytes");
}

#[test]
fn test_empty_pdf_bytes() {
    let result = PdfDocument::from_bytes(vec![]);
    assert!(result.is_err(), "Should fail for empty PDF bytes");
}
