#![allow(clippy::expect_used, clippy::similar_names, clippy::unwrap_used)]

//! Integration tests for pdf-translator-core
//!
//! These tests verify the end-to-end workflow:
//! - PDF loading and text extraction
//! - Translation with mock backend
//! - Cache hits and misses
//! - PDF overlay creation

use async_trait::async_trait;
use lopdf::{Dictionary, Document as LoDocument, Object, Stream};
use pdf_translator_core::{
    AppConfig, CacheKey, Error, Lang, OverlayOptions, PdfDocument, PdfOverlay, PdfTranslator,
    Result, Translator, TranslatorCacheIdentity, translator::TranslatorInfo,
};
use std::sync::Arc;

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

    const fn failing() -> Self {
        Self {
            prefix: String::new(),
            should_fail: true,
        }
    }
}

#[async_trait]
impl Translator for MockTranslator {
    fn cache_identity(&self) -> TranslatorCacheIdentity {
        TranslatorCacheIdentity::new("mock", "local", "deterministic")
    }
    async fn translate(&self, text: &str, _source: &Lang, _target: &Lang) -> Result<String> {
        if self.should_fail {
            return Err(Error::TranslationRequest(
                "Mock translation failure".to_string(),
            ));
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
    PdfDocument::from_bytes(pdf_bytes.to_vec()).expect("Failed to load test PDF")
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

fn inherited_geometry_pdf() -> Vec<u8> {
    let mut doc = LoDocument::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
    let resources_id = doc.add_object(Dictionary::new());
    let page_id = doc.add_object(Dictionary::from_iter([
        ("Type", Object::Name(b"Page".to_vec())),
        ("Parent", Object::Reference(pages_id)),
        ("Contents", Object::Reference(content_id)),
    ]));
    doc.objects.insert(
        pages_id,
        Object::Dictionary(Dictionary::from_iter([
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(vec![Object::Reference(page_id)])),
            ("Count", Object::Integer(1)),
            (
                "MediaBox",
                Object::Array(vec![0.into(), 0.into(), 400.into(), 600.into()]),
            ),
            (
                "CropBox",
                Object::Array(vec![10.into(), 20.into(), 390.into(), 580.into()]),
            ),
            ("Rotate", Object::Integer(90)),
            ("Resources", Object::Reference(resources_id)),
        ])),
    );
    let catalog_id = doc.add_object(Dictionary::from_iter([
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(pages_id)),
    ]));
    doc.trailer.set("Root", Object::Reference(catalog_id));
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("generated PDF should save");
    bytes
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
    assert!(
        doc.page_count() >= 1,
        "Test PDF should have at least 1 page"
    );
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

    let pdf_translator =
        PdfTranslator::with_translator(translator, config).expect("Should create translator");

    let result = pdf_translator.translate_page(&doc, 0).await;
    assert!(
        result.is_ok(),
        "Translation should succeed: {:?}",
        result.err()
    );

    let translated = result.unwrap();
    assert_eq!(translated.page_num, 0);
    assert!(
        !translated.pdf_bytes.is_empty(),
        "Should produce PDF output"
    );
    assert!(
        !translated.from_cache,
        "First translation should not be from cache"
    );
}

#[tokio::test]
async fn test_translation_error_handling() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::failing());

    let pdf_translator =
        PdfTranslator::with_translator(translator, config).expect("Should create translator");

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

    let pdf_translator =
        PdfTranslator::with_translator(translator, config).expect("Should create translator");

    // First translation - should not be cached
    let first = pdf_translator
        .translate_page(&doc, 0)
        .await
        .expect("First translation should succeed");
    assert!(
        !first.from_cache,
        "First translation should not be from cache"
    );

    // Second translation - should be cached
    let second = pdf_translator
        .translate_page(&doc, 0)
        .await
        .expect("Second translation should succeed");
    assert!(second.from_cache, "Second translation should be from cache");

    // Both should produce the same output
    assert_eq!(
        first.pdf_bytes, second.pdf_bytes,
        "Cached result should match original"
    );
}

#[tokio::test]
async fn test_force_bypasses_cache() {
    let doc = load_test_pdf();
    let config = test_config();
    let translator = Arc::new(MockTranslator::new());

    let pdf_translator =
        PdfTranslator::with_translator(translator, config).expect("Should create translator");

    // First translation
    let _ = pdf_translator
        .translate_page(&doc, 0)
        .await
        .expect("First translation should succeed");

    // Force re-translation - should bypass cache
    let forced = pdf_translator
        .translate_page_force(&doc, 0, true)
        .await
        .expect("Forced translation should succeed");
    assert!(!forced.from_cache, "Forced translation should bypass cache");
}

#[tokio::test]
async fn test_cache_key_uniqueness() {
    let lang_en = Lang::new("en");
    let lang_fr = Lang::new("fr");

    let translator = TranslatorCacheIdentity::new("mock", "local", "deterministic");
    let color = pdf_translator_core::TextColor::default();
    let key1 = CacheKey::from_page("doc1", 0, "Hello", &translator, &lang_fr, &lang_en, color);
    let key2 = CacheKey::from_page("doc1", 0, "Hello", &translator, &lang_fr, &lang_en, color);
    let key3 = CacheKey::from_page("doc1", 1, "Hello", &translator, &lang_fr, &lang_en, color);
    let key4 = CacheKey::from_page("doc1", 0, "World", &translator, &lang_fr, &lang_en, color);
    let key5 = CacheKey::from_page("doc1", 0, "Hello", &translator, &lang_en, &lang_fr, color);
    let remote = TranslatorCacheIdentity::new("mock", "remote", "deterministic");
    let other_model = TranslatorCacheIdentity::new("mock", "local", "other-model");
    let key6 = CacheKey::from_page("doc1", 0, "Hello", &remote, &lang_fr, &lang_en, color);
    let key7 = CacheKey::from_page("doc1", 0, "Hello", &other_model, &lang_fr, &lang_en, color);

    // Same inputs should produce same key
    assert_eq!(key1.as_str(), key2.as_str());

    // Different page should produce different key
    assert_ne!(key1.as_str(), key3.as_str());

    // Different content should produce different key
    assert_ne!(key1.as_str(), key4.as_str());

    // Different target language should produce different key
    assert_ne!(key1.as_str(), key5.as_str());

    assert_ne!(key1.as_str(), key6.as_str());
    assert_ne!(key1.as_str(), key7.as_str());
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
    assert!(
        result.is_ok(),
        "Should create overlay PDF: {:?}",
        result.err()
    );

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
    let overlays = vec![pdf_translator_core::pdf::overlay::TranslationOverlay {
        bbox: pdf_translator_core::pdf::BoundingBox::new(100.0, 100.0, 300.0, 120.0),
        original: "Test text".to_string(),
        translated: "Translated text".to_string(),
        font_size: 12.0,
    }];

    let result = overlay.create_translated_page(doc.bytes(), 0, &overlays);
    assert!(
        result.is_ok(),
        "Should create overlay with translations: {:?}",
        result.err()
    );
}

#[test]
fn translated_page_materializes_inherited_geometry_and_resources() {
    let input = inherited_geometry_pdf();
    let output = PdfOverlay::new(OverlayOptions::default())
        .create_translated_page(&input, 0, &[])
        .expect("inherited page should produce a standalone PDF");
    let parsed = LoDocument::load_mem(&output).expect("output should parse");
    let page_id = *parsed.get_pages().get(&1).expect("output page");
    let page = parsed
        .get_object(page_id)
        .expect("page object")
        .as_dict()
        .expect("page dictionary");

    assert!(page.get(b"MediaBox").is_ok());
    assert!(page.get(b"CropBox").is_ok());
    assert!(page.get(b"Resources").is_ok());
    assert_eq!(
        page.get(b"Rotate")
            .expect("materialized rotation")
            .as_i64()
            .expect("integer rotation"),
        90
    );
}

#[test]
fn translated_overlay_is_searchable_as_unicode() {
    let doc = load_test_pdf();
    let translated = "Žlutý 中 🙂";
    let overlays = vec![pdf_translator_core::pdf::overlay::TranslationOverlay {
        bbox: pdf_translator_core::pdf::BoundingBox::new(40.0, 40.0, 500.0, 90.0),
        original: "source".to_string(),
        translated: translated.to_string(),
        font_size: 16.0,
    }];
    let output = PdfOverlay::new(OverlayOptions::default())
        .create_translated_page(doc.bytes(), 0, &overlays)
        .expect("Unicode overlay should be created");
    let output_doc = PdfDocument::from_bytes(output).expect("overlay output should open");
    let blocks = pdf_translator_core::pdf::TextExtractor::new(&output_doc).extract_page_blocks(0);
    let extracted = blocks
        .expect("overlay text should extract")
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        extracted.contains(translated),
        "extracted overlay text did not preserve Unicode: {extracted:?}"
    );
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

    assert!(
        result.is_ok(),
        "Should render page as WebP: {:?}",
        result.err()
    );

    let webp_bytes = result.unwrap();
    assert!(!webp_bytes.is_empty(), "Should produce non-empty WebP");

    // Check WebP magic bytes (RIFF....WEBP)
    assert!(
        webp_bytes.starts_with(b"RIFF") && webp_bytes.len() > 12 && &webp_bytes[8..12] == b"WEBP",
        "Output should be valid WebP"
    );
}

#[test]
fn rasterization_rejects_unbounded_scale_before_allocation() {
    let doc = load_test_pdf();
    let renderer = pdf_translator_core::pdf::PageRenderer::with_scale(&doc, 1_000_000.0);
    assert!(renderer.page_size(0).is_err());
    assert!(renderer.render_page_webp(0).is_err());
}

#[test]
fn cloned_document_remains_renderable_after_original_drop() {
    let original = load_test_pdf();
    let cloned = original.clone();
    drop(original);
    let rendered = pdf_translator_core::render_page(&cloned, 0, 1.0)
        .expect("cloned document owns its source bytes");
    assert!(rendered.starts_with(&[0x89, b'P', b'N', b'G']));
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
