mod document;
mod font;
pub mod overlay;
mod page_index;
mod render;
mod text;

pub use document::{MAX_PAGE_COUNT, PdfDocument};
pub use overlay::{OverlayOptions, PdfOverlay, TranslationOverlay, combine_pdfs};
pub use page_index::PageIndex;
pub use render::{PageRenderer, PageSize, render_page_from_bytes};
pub use text::{BoundingBox, TextBlock, TextExtractor};
