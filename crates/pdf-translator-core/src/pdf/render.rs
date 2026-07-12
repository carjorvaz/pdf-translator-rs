use image::{ImageEncoder, RgbaImage};
use mupdf::{Colorspace, Matrix, Rect};
use webp::Encoder as WebpEncoder;

use super::document::PdfDocument;
use super::page_index::PageIndex;
use crate::error::{Error, Result};

/// Rendered page dimensions
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: u32,
    pub height: u32,
}

/// Default scale factor for rendering (2.0 for high DPI)
pub const DEFAULT_RENDER_SCALE: f32 = 2.0;

/// Maximum width or height supported by the WebP bitstream format.
pub const MAX_WEBP_DIMENSION: u32 = 16_383;

/// Maximum number of pixels allocated for a rendered page.
///
/// RGBA conversion alone uses four bytes per pixel, in addition to MuPDF's
/// raster and encoder working memory.
pub const MAX_RENDER_PIXELS: u64 = 32_000_000;

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
        let page = doc
            .load_page(page_index.into())
            .map_err(|e| Error::PdfRender {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            })?;

        let bounds = page.bounds().map_err(|e| Error::PdfRender {
            page: page_num,
            reason: format!("Failed to get bounds: {e}"),
        })?;

        self.validated_page_size(bounds, page_num)
    }

    /// Render a page to an RGBA image buffer
    pub fn render_page(&self, page_num: usize) -> Result<RgbaImage> {
        let page_index = PageIndex::try_from_page_num(page_num, self.doc.page_count())?;

        let doc = self.doc.open_document()?;
        let page = doc
            .load_page(page_index.into())
            .map_err(|e| Error::PdfRender {
                page: page_num,
                reason: format!("Failed to load page: {e}"),
            })?;

        let bounds = page.bounds().map_err(|e| Error::PdfRender {
            page: page_num,
            reason: format!("Failed to get bounds: {e}"),
        })?;
        let expected_size = self.validated_page_size(bounds, page_num)?;
        let matrix = Matrix::new_scale(self.scale, self.scale);

        // Rendering without an alpha channel composites onto opaque white in
        // MuPDF and avoids treating its premultiplied RGBA as straight alpha.
        let pixmap = page
            .to_pixmap(&matrix, &Colorspace::device_rgb(), 0.0, true)
            .map_err(|e| Error::PdfRender {
                page: page_num,
                reason: format!("Failed to render: {e}"),
            })?;

        let img_width = pixmap.width();
        let img_height = pixmap.height();
        if img_width != expected_size.width || img_height != expected_size.height {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "MuPDF returned unexpected raster dimensions {img_width}x{img_height}; expected {}x{}",
                    expected_size.width, expected_size.height
                ),
            });
        }

        let n = usize::from(pixmap.n());
        if !matches!(n, 1 | 3) {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!("Unexpected opaque pixel format with {n} components"),
            });
        }
        let pixel_count = usize::try_from(
            u64::from(img_width)
                .checked_mul(u64::from(img_height))
                .ok_or_else(|| Error::PdfRender {
                    page: page_num,
                    reason: "Raster pixel count overflowed".to_string(),
                })?,
        )
        .map_err(|_| Error::PdfRender {
            page: page_num,
            reason: "Raster pixel count does not fit in memory".to_string(),
        })?;
        let sample_count = pixel_count.checked_mul(n).ok_or_else(|| Error::PdfRender {
            page: page_num,
            reason: "Raster sample count overflowed".to_string(),
        })?;
        let pixels = pixmap.samples();
        if pixels.len() != sample_count {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "MuPDF returned {} samples for a {img_width}x{img_height} raster with {n} components",
                    pixels.len()
                ),
            });
        }
        let rgba_len = pixel_count.checked_mul(4).ok_or_else(|| Error::PdfRender {
            page: page_num,
            reason: "RGBA buffer size overflowed".to_string(),
        })?;
        let mut rgba_pixels = Vec::with_capacity(rgba_len);

        if n == 3 {
            for chunk in pixels.chunks_exact(3) {
                rgba_pixels.extend_from_slice(chunk);
                rgba_pixels.push(255);
            }
        } else {
            for &sample in pixels {
                rgba_pixels.extend_from_slice(&[sample, sample, sample, 255]);
            }
        }

        RgbaImage::from_raw(img_width, img_height, rgba_pixels).ok_or_else(|| Error::PdfRender {
            page: page_num,
            reason: "Failed to create image buffer".to_string(),
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

        // Use libwebp for lossy encoding (5-10x smaller than lossless).
        let encoder = WebpEncoder::from_rgba(img.as_raw(), img.width(), img.height());
        let webp_data = encoder
            .encode_simple(false, 85.0)
            .map_err(|e| Error::PdfRender {
                page: page_num,
                reason: format!("Failed to encode WebP: {e:?}"),
            })?;

        Ok(webp_data.to_vec())
    }

    fn validated_page_size(&self, bounds: Rect, page_num: usize) -> Result<PageSize> {
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "Render scale must be finite and positive, got {}",
                    self.scale
                ),
            });
        }

        let scaled = [
            bounds.x0 * self.scale,
            bounds.y0 * self.scale,
            bounds.x1 * self.scale,
            bounds.y1 * self.scale,
        ];
        if !scaled.iter().all(|coordinate| coordinate.is_finite()) {
            return Err(Error::PdfRender {
                page: page_num,
                reason: "Transformed page bounds are not finite".to_string(),
            });
        }

        // Match MuPDF's fz_round_rect so reported dimensions equal the raster.
        let left = (scaled[0] + 0.001).floor();
        let top = (scaled[1] + 0.001).floor();
        let right = (scaled[2] - 0.001).ceil();
        let bottom = (scaled[3] - 0.001).ceil();
        let min = -2_147_483_648.0_f32;
        let max = 2_147_483_648.0_f32;
        if [left, top, right, bottom]
            .iter()
            .any(|&coordinate| coordinate < min || coordinate >= max)
        {
            return Err(Error::PdfRender {
                page: page_num,
                reason: "Transformed page bounds exceed MuPDF's integer coordinate range"
                    .to_string(),
            });
        }

        #[allow(clippy::cast_possible_truncation)]
        let (left, top, right, bottom) = (left as i64, top as i64, right as i64, bottom as i64);
        let width = right.checked_sub(left).ok_or_else(|| Error::PdfRender {
            page: page_num,
            reason: "Transformed page width overflowed".to_string(),
        })?;
        let height = bottom.checked_sub(top).ok_or_else(|| Error::PdfRender {
            page: page_num,
            reason: "Transformed page height overflowed".to_string(),
        })?;
        if width <= 0 || height <= 0 {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "Transformed page dimensions must be positive, got {width}x{height}"
                ),
            });
        }

        let width = u32::try_from(width).map_err(|_| Error::PdfRender {
            page: page_num,
            reason: "Transformed page width is too large".to_string(),
        })?;
        let height = u32::try_from(height).map_err(|_| Error::PdfRender {
            page: page_num,
            reason: "Transformed page height is too large".to_string(),
        })?;
        if width > MAX_WEBP_DIMENSION || height > MAX_WEBP_DIMENSION {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "Raster dimensions {width}x{height} exceed the WebP limit of {MAX_WEBP_DIMENSION}"
                ),
            });
        }

        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| Error::PdfRender {
                page: page_num,
                reason: "Raster pixel count overflowed".to_string(),
            })?;
        if pixels > MAX_RENDER_PIXELS {
            return Err(Error::PdfRender {
                page: page_num,
                reason: format!(
                    "Raster contains {pixels} pixels, exceeding the limit of {MAX_RENDER_PIXELS}"
                ),
            });
        }

        Ok(PageSize { width, height })
    }
}

/// Convenience function to render a single page from bytes
pub fn render_page_from_bytes(pdf_bytes: &[u8], page_num: usize, scale: f32) -> Result<Vec<u8>> {
    let doc = PdfDocument::from_bytes(pdf_bytes.to_vec())?;
    let renderer = PageRenderer::with_scale(&doc, scale);
    renderer.render_page_png(page_num)
}
