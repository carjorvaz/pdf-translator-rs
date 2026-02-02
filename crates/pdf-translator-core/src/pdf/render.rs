use image::{ImageEncoder, RgbaImage};
use mupdf::{Colorspace, Matrix};
use webp::Encoder as WebpEncoder;

use crate::error::{Error, Result};
use super::document::PdfDocument;
use super::page_index::PageIndex;

/// Rendered page dimensions
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: u32,
    pub height: u32,
}

/// Default scale factor for rendering (2.0 for high DPI)
pub const DEFAULT_RENDER_SCALE: f32 = 2.0;

/// Page renderer for PDF documents
pub struct PageRenderer<'a> {
    /// The PDF document to render
    pub doc: &'a PdfDocument,
    /// Scale factor for rendering
    pub scale: f32,
}

impl<'a> PageRenderer<'a> {
    /// Create a renderer with default scale (2.0)
    pub const fn new(doc: &'a PdfDocument) -> Self {
        Self {
            doc,
            scale: DEFAULT_RENDER_SCALE,
        }
    }

    /// Create a renderer with custom scale
    pub const fn with_scale(doc: &'a PdfDocument, scale: f32) -> Self {
        Self { doc, scale }
    }

    /// Get the size of a page at the current scale
    pub fn page_size(&self, page_num: usize) -> Result<PageSize> {
        let page_index = PageIndex::try_from_page_num(page_num, self.doc.page_count())?;

        let doc = self.doc.open_document()?;
        let page = doc.load_page(page_index.into()).map_err(|e| {
            Error::PdfRender {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            }
        })?;

        let bounds = page.bounds().map_err(|e| {
            Error::PdfRender {
                page: page_num,
                reason: format!("Failed to get bounds: {e}"),
            }
        })?;

        // PDF dimensions are always positive and reasonable (< millions of pixels)
        let width = f32_to_u32((bounds.x1 - bounds.x0) * self.scale);
        let height = f32_to_u32((bounds.y1 - bounds.y0) * self.scale);

        Ok(PageSize { width, height })
    }

    /// Render a page to an RGBA image buffer
    pub fn render_page(&self, page_num: usize) -> Result<RgbaImage> {
        let page_index = PageIndex::try_from_page_num(page_num, self.doc.page_count())?;

        let doc = self.doc.open_document()?;
        let page = doc.load_page(page_index.into()).map_err(|e| {
            Error::PdfRender {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            }
        })?;

        let _bounds = page.bounds().map_err(|e| {
            Error::PdfRender {
                page: page_num,
                reason: format!("Failed to get bounds: {e}"),
            }
        })?;

        // Create transformation matrix for scaling
        let matrix = Matrix::new_scale(self.scale, self.scale);

        // Render to pixmap (RGBA)
        let pixmap = page
            .to_pixmap(&matrix, &Colorspace::device_rgb(), 1.0, true)
            .map_err(|e| {
                Error::PdfRender {
                    page: page_num,
                    reason: format!("Failed to render: {e}"),
                }
            })?;

        // Convert to image
        let pixels = pixmap.samples();
        let img_width = pixmap.width();
        let img_height = pixmap.height();

        // mupdf returns RGB, we need RGBA
        let n = pixmap.n() as usize; // components per pixel
        let mut rgba_pixels = Vec::with_capacity((img_width * img_height * 4) as usize);

        for chunk in pixels.chunks(n) {
            match n {
                3 => {
                    // RGB -> RGBA
                    rgba_pixels.push(chunk[0]);
                    rgba_pixels.push(chunk[1]);
                    rgba_pixels.push(chunk[2]);
                    rgba_pixels.push(255);
                }
                4 => {
                    // Already RGBA
                    rgba_pixels.extend_from_slice(chunk);
                }
                1 => {
                    // Grayscale -> RGBA
                    rgba_pixels.push(chunk[0]);
                    rgba_pixels.push(chunk[0]);
                    rgba_pixels.push(chunk[0]);
                    rgba_pixels.push(255);
                }
                _ => {
                    return Err(Error::PdfRender {
                        page: page_num,
                        reason: format!("Unexpected pixel format with {n} components"),
                    });
                }
            }
        }

        RgbaImage::from_raw(img_width, img_height, rgba_pixels).ok_or_else(|| {
            Error::PdfRender {
                page: page_num,
                reason: "Failed to create image buffer".to_string(),
            }
        })
    }

    /// Render a page to PNG bytes
    pub fn render_page_png(&self, page_num: usize) -> Result<Vec<u8>> {
        let img = self.render_page(page_num)?;

        let mut png_data = Vec::new();
        // Use fast compression for better performance (still lossless)
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut png_data,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );

        encoder
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| Error::PdfRender {
                page: page_num,
                reason: format!("Failed to encode PNG: {e}"),
            })?;

        Ok(png_data)
    }

    /// Render a page to WebP bytes (lossy, quality 85 - good balance of size and quality)
    pub fn render_page_webp(&self, page_num: usize) -> Result<Vec<u8>> {
        let img = self.render_page(page_num)?;

        // Use libwebp for lossy encoding (5-10x smaller than lossless)
        let encoder = WebpEncoder::from_rgba(img.as_raw(), img.width(), img.height());
        let webp_data = encoder.encode(85.0); // Quality 85: good balance of size and quality

        Ok(webp_data.to_vec())
    }
}

/// Convenience function to render a single page from bytes
pub fn render_page_from_bytes(pdf_bytes: &[u8], page_num: usize, scale: f32) -> Result<Vec<u8>> {
    let doc = PdfDocument::from_bytes(pdf_bytes.to_vec())?;
    let renderer = PageRenderer::with_scale(&doc, scale);
    renderer.render_page_png(page_num)
}

/// Convert f32 dimension to u32, clamping to valid range.
/// PDF dimensions are always non-negative and reasonable for rendering.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
const fn f32_to_u32(value: f32) -> u32 {
    // Precision loss on MAX is fine - we just need an upper bound
    const MAX: f32 = u32::MAX as f32;
    // Manual clamp since f32::clamp isn't const
    let clamped = if value < 0.0 {
        0.0
    } else if value > MAX {
        MAX
    } else {
        value
    };
    clamped as u32
}
