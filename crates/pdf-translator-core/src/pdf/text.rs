use mupdf::TextPageOptions;

use crate::error::{Error, Result};
use super::document::PdfDocument;
use super::page_index::PageIndex;

/// A text block extracted from a PDF page with bounding box
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// The text content
    pub text: String,
    /// Bounding box: (x0, y0, x1, y1) in PDF coordinates
    pub bbox: BoundingBox,
    /// Font size (estimated from line height)
    pub font_size: f32,
    /// Number of lines in the original text
    pub line_count: usize,
}

/// Bounding box in PDF coordinates
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BoundingBox {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Convert to array format [x0, y0, x1, y1]
    pub const fn as_array(self) -> [f32; 4] {
        [self.x0, self.y0, self.x1, self.y1]
    }

    /// Create from mupdf Rect
    pub const fn from_rect(rect: mupdf::Rect) -> Self {
        Self {
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
        }
    }

    /// Create from mupdf Quad (4 points defining a quadrilateral)
    pub const fn from_quad(quad: &mupdf::Quad) -> Self {
        // Calculate bounding box from quad points
        let x0 = quad.ul.x.min(quad.ur.x).min(quad.ll.x).min(quad.lr.x);
        let y0 = quad.ul.y.min(quad.ur.y).min(quad.ll.y).min(quad.lr.y);
        let x1 = quad.ul.x.max(quad.ur.x).max(quad.ll.x).max(quad.lr.x);
        let y1 = quad.ul.y.max(quad.ur.y).max(quad.ll.y).max(quad.lr.y);
        Self { x0, y0, x1, y1 }
    }
}

/// Text extraction from PDF pages
pub struct TextExtractor<'a> {
    /// The PDF document to extract text from
    pub doc: &'a PdfDocument,
    /// Dehyphenate text (join hyphenated words across lines)
    pub dehyphenate: bool,
    /// Minimum text length to include
    pub min_length: usize,
}

impl<'a> TextExtractor<'a> {
    /// Create a new text extractor with default options
    pub const fn new(doc: &'a PdfDocument) -> Self {
        Self {
            doc,
            dehyphenate: false,
            min_length: 0,
        }
    }

    /// Extract text blocks from a page (similar to PyMuPDF's get_text("blocks"))
    ///
    /// Each mupdf "block" represents a paragraph, so we group all lines within
    /// a block together rather than treating each line separately.
    pub fn extract_page_blocks(&self, page_num: usize) -> Result<Vec<TextBlock>> {
        let page_index = PageIndex::try_from_page_num(page_num, self.doc.page_count())?;

        let doc = self.doc.open_document()?;
        let page = doc.load_page(page_index.into()).map_err(|e| {
            Error::PdfTextExtraction {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            }
        })?;

        // Get text page (mupdf doesn't have a dehyphenate option)
        let flags = TextPageOptions::empty();

        let text_page = page.to_text_page(flags).map_err(|e| {
            Error::PdfTextExtraction {
                page: page_num,
                reason: format!("Failed to get text page: {e}"),
            }
        })?;

        let mut blocks = Vec::new();

        // Iterate through text blocks (paragraphs)
        for block in text_page.blocks() {
            let mut block_text = String::new();
            let mut block_bbox: Option<BoundingBox> = None;
            let mut line_count: usize = 0;
            let mut line_heights: Vec<f32> = Vec::new();

            // Collect all lines in this block as one paragraph
            for line in block.lines() {
                let mut line_text = String::new();
                let mut line_bbox: Option<BoundingBox> = None;

                for text_char in line.chars() {
                    // char() returns Option<char>
                    if let Some(c) = text_char.char() {
                        line_text.push(c);
                    }

                    // Use quad() to get character bounding box
                    let char_bbox = BoundingBox::from_quad(&text_char.quad());

                    // Track line bbox for font size estimation
                    line_bbox = Some(line_bbox.map_or(char_bbox, |bbox| BoundingBox {
                        x0: bbox.x0.min(char_bbox.x0),
                        y0: bbox.y0.min(char_bbox.y0),
                        x1: bbox.x1.max(char_bbox.x1),
                        y1: bbox.y1.max(char_bbox.y1),
                    }));

                    block_bbox = Some(block_bbox.map_or(char_bbox, |bbox| BoundingBox {
                        x0: bbox.x0.min(char_bbox.x0),
                        y0: bbox.y0.min(char_bbox.y0),
                        x1: bbox.x1.max(char_bbox.x1),
                        y1: bbox.y1.max(char_bbox.y1),
                    }));
                }

                let line_trimmed = line_text.trim();
                if line_trimmed.is_empty() {
                    continue;
                }

                // Track line height for font size estimation
                if let Some(lb) = line_bbox {
                    line_heights.push(lb.height());
                }
                line_count += 1;

                // Join lines: handle hyphenation at line breaks
                if block_text.ends_with('-') {
                    // Remove hyphen and join directly (dehyphenate)
                    block_text.pop();
                } else if !block_text.is_empty() {
                    // Add space between lines
                    block_text.push(' ');
                }
                block_text.push_str(line_trimmed);
            }

            let text = block_text.trim().to_string();

            // Filter out tiny fragments (likely page numbers, artifacts, etc.)
            // Use min_length if set, otherwise default to 3 chars minimum
            let min_len = if self.min_length > 0 { self.min_length } else { 3 };
            if text.is_empty() || text.len() < min_len {
                continue;
            }

            if let Some(bbox) = block_bbox {
                // Estimate font size from average character height
                // The line bbox height from mupdf tends to be slightly smaller than
                // the visual font size, so we scale up slightly to match better
                #[allow(clippy::cast_precision_loss)] // Line counts don't need f64 precision
                let avg_char_height = if line_heights.is_empty() {
                    bbox.height() / line_count.max(1) as f32
                } else {
                    line_heights.iter().sum::<f32>() / line_heights.len() as f32
                };
                // Scale up slightly to better match original visual size
                // Allow wider range for headings and small text
                let font_size = (avg_char_height * 1.18).clamp(6.0, 36.0);

                blocks.push(TextBlock {
                    text,
                    bbox,
                    font_size,
                    line_count,
                });
            }
        }

        // Merge blocks that are split by hyphenation
        let blocks = Self::merge_hyphenated_blocks(blocks);

        Ok(blocks)
    }

    /// Merge adjacent blocks where one ends with a hyphen and the next continues the word.
    /// This handles cases where MuPDF splits hyphenated words across different blocks.
    fn merge_hyphenated_blocks(mut blocks: Vec<TextBlock>) -> Vec<TextBlock> {
        if blocks.len() < 2 {
            return blocks;
        }

        // Sort blocks by vertical position (top to bottom in page coordinates)
        // In MuPDF coords, smaller y0 = higher on page
        blocks.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap_or(std::cmp::Ordering::Equal));

        let mut merged: Vec<TextBlock> = Vec::with_capacity(blocks.len());
        let mut i = 0;

        while i < blocks.len() {
            let mut current = blocks[i].clone();

            // Look ahead and merge any continuation blocks
            while i + 1 < blocks.len() {
                let next = &blocks[i + 1];

                // Check if current block ends with hyphen (trim whitespace first)
                let current_trimmed = current.text.trim_end();
                let current_ends_hyphen = current_trimmed.ends_with('-');

                // Check if next block looks like a word fragment (starts lowercase, relatively short)
                let next_trimmed = next.text.trim_start();
                let next_starts_lower = next_trimmed.chars().next()
                    .map(|c| c.is_lowercase())
                    .unwrap_or(false);
                let next_is_fragment = next_trimmed.len() < 20 && !next_trimmed.contains(' ');

                // Calculate vertical gap - use absolute value to handle overlapping blocks
                let vertical_gap = (next.bbox.y0 - current.bbox.y1).abs();
                let avg_height = (current.bbox.height() + next.bbox.height()) / 2.0;
                // Be more generous with vertical distance - allow up to 3x line height
                let close_vertically = vertical_gap < avg_height * 3.0;

                // Merge if: current ends with hyphen AND (next starts lowercase OR is a short fragment)
                // AND they're vertically close
                let should_merge = current_ends_hyphen
                    && (next_starts_lower || next_is_fragment)
                    && close_vertically;

                if should_merge {
                    // Merge: remove trailing whitespace and hyphen, then join
                    let trimmed = current.text.trim_end();
                    let without_hyphen = trimmed.strip_suffix('-').unwrap_or(trimmed);
                    current.text = format!("{}{}", without_hyphen, next.text.trim_start());

                    // Expand bounding box
                    current.bbox = BoundingBox {
                        x0: current.bbox.x0.min(next.bbox.x0),
                        y0: current.bbox.y0.min(next.bbox.y0),
                        x1: current.bbox.x1.max(next.bbox.x1),
                        y1: current.bbox.y1.max(next.bbox.y1),
                    };

                    // Update line count and use smaller font size
                    current.line_count += next.line_count;
                    current.font_size = current.font_size.min(next.font_size);

                    i += 1; // Skip the merged block
                } else {
                    break;
                }
            }

            merged.push(current);
            i += 1;
        }

        merged
    }

    /// Get plain text from a page (for cache key generation)
    pub fn get_page_text(&self, page_num: usize) -> Result<String> {
        let page_index = PageIndex::try_from_page_num(page_num, self.doc.page_count())?;

        let doc = self.doc.open_document()?;
        let page = doc.load_page(page_index.into()).map_err(|e| {
            Error::PdfTextExtraction {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            }
        })?;

        let flags = TextPageOptions::empty();

        let text_page = page.to_text_page(flags).map_err(|e| {
            Error::PdfTextExtraction {
                page: page_num,
                reason: format!("Failed to get text page: {e}"),
            }
        })?;

        // Collect all text
        let mut all_text = String::new();
        for block in text_page.blocks() {
            for line in block.lines() {
                for text_char in line.chars() {
                    if let Some(c) = text_char.char() {
                        all_text.push(c);
                    }
                }
                all_text.push('\n');
            }
        }

        Ok(all_text)
    }
}

