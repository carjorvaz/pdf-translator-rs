//! PDF overlay creation for translation text.
//!
//! # Coordinate System
//!
//! PDF uses a **bottom-left origin** coordinate system where:
//! - (0, 0) is at the bottom-left corner of the page
//! - X increases to the right
//! - Y increases upward
//!
//! However, MuPDF (used for text extraction) uses a **top-left origin**:
//! - (0, 0) is at the top-left corner
//! - Y increases downward
//!
//! This module handles the conversion between these two systems when
//! positioning translation overlays. The formula is:
//! ```text
//! pdf_y = page_height - mupdf_y
//! ```
//!
//! # Overlay Strategy
//!
//! Simple two-phase rendering:
//! 1. Draw white rectangles to cover original text
//! 2. Draw translated text at consistent font size

use lopdf::{Document, Object, ObjectId, Stream};

use crate::config::TextColor;
use crate::error::{Error, Result};
use super::font::EmbeddedFont;
use super::page_index::PageIndex;
use super::text::BoundingBox;

// =============================================================================
// Layout Constants
// =============================================================================

/// Default font size for translations (in points).
/// A readable size that works well for most documents.
const DEFAULT_FONT_SIZE: f32 = 13.0;

/// Line height as a multiple of font size.
const LINE_HEIGHT_FACTOR: f32 = 1.25;

/// Average character width as a fraction of font size for Noto Serif.
const CHAR_WIDTH_FACTOR: f32 = 0.55;

/// Horizontal padding on left side of cover rectangles (in points).
const RECT_LEFT_PADDING: f32 = 5.0;

/// Horizontal padding on right side of cover rectangles (in points).
const RECT_RIGHT_PADDING: f32 = 10.0;

/// Vertical padding above text in cover rectangle (in points).
const RECT_TOP_PADDING: f32 = 3.0;

/// Vertical padding below text in cover rectangle (in points).
const RECT_BOTTOM_PADDING: f32 = 3.0;

/// Right margin from page edge for text wrapping (in points).
const PAGE_RIGHT_MARGIN: f32 = 40.0;

// =============================================================================
// Public Types
// =============================================================================

/// Options for PDF overlay creation
#[derive(Debug, Clone)]
pub struct OverlayOptions {
    /// Text color for translations
    pub text_color: TextColor,
    /// Font size for translations (if None, uses DEFAULT_FONT_SIZE)
    pub font_size: Option<f32>,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            text_color: TextColor::default(),
            font_size: None,
        }
    }
}

/// A translation overlay to be applied to a PDF.
#[derive(Debug, Clone)]
pub struct TranslationOverlay {
    /// Bounding box where to place the text (in MuPDF coordinates).
    pub bbox: BoundingBox,
    /// Original text (kept for debugging/logging purposes)
    pub original: String,
    /// Translated text to render
    pub translated: String,
    /// Font size in points (estimated from original text metrics)
    pub font_size: f32,
}

// =============================================================================
// Render Data (simplified)
// =============================================================================

/// Pre-calculated data for rendering a single text overlay.
struct RenderBlock {
    /// White rectangle position and size
    rect_x: f32,
    rect_y: f32,
    rect_width: f32,
    rect_height: f32,
    /// Text position
    text_x: f32,
    text_start_y: f32,
    /// Text properties
    font_size: f32,
    line_height: f32,
    lines: Vec<String>,
}

impl RenderBlock {
    /// Create render data from an overlay.
    fn from_overlay(
        overlay: &TranslationOverlay,
        page_height: f32,
        page_width: f32,
        font_size: f32,
    ) -> Self {
        let x = overlay.bbox.x0;
        // Convert Y: PDF has origin at bottom-left, MuPDF at top-left
        let top_y = page_height - overlay.bbox.y0;
        let original_width = overlay.bbox.x1 - overlay.bbox.x0;
        let original_height = overlay.bbox.y1 - overlay.bbox.y0;

        // Calculate max width for word wrapping
        let max_width = (page_width - x - PAGE_RIGHT_MARGIN).max(100.0);

        // Word wrap the text
        let char_width = font_size * CHAR_WIDTH_FACTOR;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let max_chars = (max_width / char_width).floor().max(10.0) as usize;
        let lines = word_wrap(&overlay.translated, max_chars);

        // Calculate rendered dimensions
        let line_height = font_size * LINE_HEIGHT_FACTOR;
        #[allow(clippy::cast_precision_loss)]
        let text_height = lines.len() as f32 * line_height;

        let longest_line_chars = lines.iter().map(|l| l.len()).max().unwrap_or(0);
        #[allow(clippy::cast_precision_loss)]
        let rendered_width = longest_line_chars as f32 * char_width;

        // Rectangle covers original text area (minimum) or rendered text (if wider)
        let rect_width = original_width.max(rendered_width) + RECT_LEFT_PADDING + RECT_RIGHT_PADDING;
        let rect_height = original_height.max(text_height) + RECT_TOP_PADDING + RECT_BOTTOM_PADDING;

        let rect_x = (x - RECT_LEFT_PADDING).max(0.0);
        let rect_y = top_y - rect_height + RECT_TOP_PADDING;

        let text_start_y = top_y - RECT_TOP_PADDING - font_size;

        Self {
            rect_x,
            rect_y,
            rect_width: rect_width.min(page_width - rect_x),
            rect_height,
            text_x: x,
            text_start_y,
            font_size,
            line_height,
            lines,
        }
    }
}

// =============================================================================
// PDF Overlay Creator
// =============================================================================

/// PDF overlay creator using lopdf.
pub struct PdfOverlay {
    /// Configuration options for overlay creation
    pub options: OverlayOptions,
    /// Embedded font for Unicode text rendering
    font: EmbeddedFont,
}

impl PdfOverlay {
    /// Create a new overlay creator with the given options.
    pub fn new(options: OverlayOptions) -> Result<Self> {
        let font = EmbeddedFont::new()?;
        Ok(Self { options, font })
    }

    /// Apply overlays to a PDF document.
    /// Returns the modified PDF bytes.
    pub fn apply_overlays(
        &self,
        pdf_bytes: &[u8],
        page_num: usize,
        overlays: &[TranslationOverlay],
    ) -> Result<Vec<u8>> {
        let mut doc = Document::load_mem(pdf_bytes)
            .map_err(|e| Error::Lopdf(format!("Failed to load PDF: {e}")))?;

        let pages = doc.get_pages();
        let page_index = PageIndex::try_from_page_num(page_num, pages.len())?;

        let page_id = pages.get(&page_index.as_lopdf_page_number()).ok_or(Error::PdfInvalidPage {
            page: page_num,
            total: pages.len(),
        })?;
        let page_id = *page_id;

        let page_obj = doc.get_object(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page object: {e}")))?;

        let media_box = get_media_box(&doc, page_obj)?;

        // Embed the Unicode font in the document
        self.font.embed_in_document(&mut doc, page_id)?;

        // Create content stream for overlays
        let overlay_content = self.create_overlay_content(overlays, &media_box);

        // Append to page content
        Self::append_content_to_page(&mut doc, page_id, &overlay_content)?;

        // Save document
        let mut output = Vec::new();
        doc.save_to(&mut output)
            .map_err(|e| Error::PdfSave(format!("Failed to save PDF: {e}")))?;

        Ok(output)
    }

    /// Create a new single-page PDF with translations overlaid.
    pub fn create_translated_page(
        &self,
        pdf_bytes: &[u8],
        page_num: usize,
        overlays: &[TranslationOverlay],
    ) -> Result<Vec<u8>> {
        self.apply_overlays(pdf_bytes, page_num, overlays)
    }

    /// Create PDF content stream for overlays.
    fn create_overlay_content(&self, overlays: &[TranslationOverlay], media_box: &[f32; 4]) -> String {
        use std::fmt::Write;

        let page_width = media_box[2] - media_box[0];
        let page_height = media_box[3] - media_box[1];
        let font_size = self.options.font_size.unwrap_or(DEFAULT_FONT_SIZE);

        // Convert overlays to render blocks
        let blocks: Vec<RenderBlock> = overlays
            .iter()
            .map(|o| RenderBlock::from_overlay(o, page_height, page_width, font_size))
            .collect();

        let mut content = String::new();

        // Save graphics state
        content.push_str("q\n");

        // PHASE 1: Draw ALL white rectangles first to cover original text
        content.push_str("1 1 1 rg\n");
        for block in &blocks {
            let _ = writeln!(
                content,
                "{} {} {} {} re f",
                block.rect_x, block.rect_y, block.rect_width, block.rect_height
            );
        }

        // PHASE 2: Draw ALL translated text on top
        let (r, g, b) = (
            self.options.text_color.r,
            self.options.text_color.g,
            self.options.text_color.b,
        );
        let _ = writeln!(content, "{r} {g} {b} rg");
        // Reset text rendering mode to fill (0) - OCR layers use invisible mode (3)
        content.push_str("0 Tr\n");

        for block in &blocks {
            for (j, line) in block.lines.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let y = block.text_start_y - (j as f32 * block.line_height);

                content.push_str("BT\n");
                let _ = writeln!(content, "/FTrans {} Tf", block.font_size);
                let _ = writeln!(content, "{} {} Td", block.text_x, y);
                let hex_glyphs = self.font.text_to_hex_glyphs(line);
                let _ = writeln!(content, "<{hex_glyphs}> Tj");
                content.push_str("ET\n");
            }
        }

        // Restore graphics state
        content.push_str("Q\n");

        content
    }

    /// Append content stream to a page.
    fn append_content_to_page(
        doc: &mut Document,
        page_id: ObjectId,
        content: &str,
    ) -> Result<()> {
        let content_stream = Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        );

        let content_id = doc.add_object(Object::Stream(content_stream));

        let page = doc.get_object_mut(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page: {e}")))?;

        if let Object::Dictionary(dict) = page {
            let existing_contents = dict.get(b"Contents").ok().cloned();

            match existing_contents {
                Some(Object::Reference(existing_id)) => {
                    let contents_array = Object::Array(vec![
                        Object::Reference(existing_id),
                        Object::Reference(content_id),
                    ]);
                    dict.set("Contents", contents_array);
                }
                Some(Object::Array(mut arr)) => {
                    arr.push(Object::Reference(content_id));
                    dict.set("Contents", Object::Array(arr));
                }
                _ => {
                    dict.set("Contents", Object::Reference(content_id));
                }
            }
        }

        Ok(())
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Get media box from page object.
fn get_media_box(doc: &Document, page_obj: &Object) -> Result<[f32; 4]> {
    if let Object::Dictionary(dict) = page_obj {
        if let Ok(Object::Array(arr)) = dict.get(b"MediaBox")
            && arr.len() == 4 {
                let values: Vec<f32> = arr
                    .iter()
                    .filter_map(|o| match o {
                        #[allow(clippy::cast_precision_loss)]
                        Object::Integer(i) => Some(*i as f32),
                        Object::Real(r) => Some(*r),
                        _ => None,
                    })
                    .collect();

                if values.len() == 4 {
                    return Ok([values[0], values[1], values[2], values[3]]);
                }
            }

        if let Ok(Object::Reference(parent_id)) = dict.get(b"Parent")
            && let Ok(parent) = doc.get_object(*parent_id) {
                return get_media_box(doc, parent);
            }
    }

    // Default to US Letter size
    Ok([0.0, 0.0, 612.0, 792.0])
}

/// Word wrap text to fit within max_chars per line.
fn word_wrap(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= max_chars {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

// =============================================================================
// PDF Combining
// =============================================================================

/// Combine multiple single-page PDFs into one document.
pub fn combine_pdfs(pages: &[Vec<u8>]) -> Result<Vec<u8>> {
    use std::collections::BTreeMap;

    if pages.is_empty() {
        return Err(Error::PdfOverlay("No pages to combine".to_string()));
    }

    if pages.len() == 1 {
        return Ok(pages[0].clone());
    }

    let mut max_id: u32 = 1;
    let mut documents_pages: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut documents_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut document = Document::with_version("1.5");

    for (i, page_bytes) in pages.iter().enumerate() {
        let mut doc = Document::load_mem(page_bytes)
            .map_err(|e| Error::Lopdf(format!("Failed to load page {}: {}", i + 1, e)))?;

        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;

        let source_pages = doc.get_pages();
        for &page_id in source_pages.values() {
            if let Ok(page_obj) = doc.get_object(page_id) {
                documents_pages.insert(page_id, page_obj.clone());
            }
        }

        for (object_id, object) in doc.objects {
            match object.type_name().unwrap_or(b"") {
                b"Catalog" | b"Pages" | b"Page" | b"Outlines" | b"Outline" => {}
                _ => {
                    documents_objects.insert(object_id, object);
                }
            }
        }
    }

    for (object_id, object) in documents_objects {
        document.objects.insert(object_id, object);
    }

    let pages_id = document.new_object_id();

    for (obj_id, object) in &documents_pages {
        if let Object::Dictionary(dict) = object {
            let mut new_dict = dict.clone();
            new_dict.set("Parent", Object::Reference(pages_id));
            document.objects.insert(*obj_id, Object::Dictionary(new_dict));
        }
    }

    let kids: Vec<Object> = documents_pages
        .keys()
        .map(|&id| Object::Reference(id))
        .collect();

    #[allow(clippy::cast_possible_truncation)]
    let total_pages = documents_pages.len() as u32;

    let pages_dict_obj = lopdf::Dictionary::from_iter([
        ("Type", Object::Name(b"Pages".to_vec())),
        ("Kids", Object::Array(kids)),
        ("Count", Object::Integer(i64::from(total_pages))),
    ]);
    document.objects.insert(pages_id, Object::Dictionary(pages_dict_obj));

    let catalog_id = document.new_object_id();
    let catalog_dict_obj = lopdf::Dictionary::from_iter([
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(pages_id)),
    ]);
    document.objects.insert(catalog_id, Object::Dictionary(catalog_dict_obj));

    document.trailer.set("Root", Object::Reference(catalog_id));

    #[allow(clippy::cast_possible_truncation)]
    let new_max_id = document.objects.len() as u32;
    document.max_id = new_max_id;

    document.renumber_objects();
    document.compress();

    let mut output = Vec::new();
    document.save_to(&mut output)
        .map_err(|e| Error::PdfSave(format!("Failed to save combined PDF: {e}")))?;

    Ok(output)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};

    fn create_test_pdf(page_text: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let page_tree_id = doc.new_object_id();

        let font_id = doc.add_object(lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
        ]));

        let resources_id = doc.add_object(lopdf::Dictionary::from_iter([(
            "Font",
            Object::Dictionary(lopdf::Dictionary::from_iter([(
                "F1",
                Object::Reference(font_id),
            )])),
        )]));

        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(page_text)]),
                Operation::new("ET", vec![]),
            ],
        };

        let content_bytes = content.encode().unwrap_or_default();
        let content_id = doc.add_object(Stream::new(lopdf::Dictionary::new(), content_bytes));

        let single_page_id = doc.add_object(lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(page_tree_id)),
            ("Contents", Object::Reference(content_id)),
            ("Resources", Object::Reference(resources_id)),
            (
                "MediaBox",
                Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
            ),
        ]));

        let page_tree = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(vec![Object::Reference(single_page_id)])),
            ("Count", Object::Integer(1)),
        ]);
        doc.objects.insert(page_tree_id, Object::Dictionary(page_tree));

        let catalog_id = doc.add_object(lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(page_tree_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut output = Vec::new();
        doc.save_to(&mut output).unwrap_or_default();
        output
    }

    #[test]
    fn test_word_wrap_basic() {
        let lines = word_wrap("Hello world this is a test", 10);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "Hello");
        assert_eq!(lines[1], "world this");
        assert_eq!(lines[2], "is a test");
    }

    #[test]
    fn test_word_wrap_empty() {
        let lines = word_wrap("", 10);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "");
    }

    #[test]
    fn test_combine_pdfs_empty() {
        let result = combine_pdfs(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_combine_pdfs_single() {
        let pdf1 = create_test_pdf("Page 1");
        let result = combine_pdfs(std::slice::from_ref(&pdf1));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), pdf1);
    }

    #[test]
    fn test_combine_pdfs_multiple() {
        let pdf1 = create_test_pdf("Page 1");
        let pdf2 = create_test_pdf("Page 2");
        let pdf3 = create_test_pdf("Page 3");

        let result = combine_pdfs(&[pdf1, pdf2, pdf3]);
        assert!(result.is_ok());

        let combined_bytes = result.unwrap();
        let combined_doc = Document::load_mem(&combined_bytes).unwrap();
        let pages = combined_doc.get_pages();
        assert_eq!(pages.len(), 3, "Combined PDF should have 3 pages");
    }
}
