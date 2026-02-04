//! HTTP route handlers for the PDF translator web application.
//!
//! All routes return either HTML (for HTMX consumption) or binary data (images, PDFs).
//! HTML routes use Askama templates from the `templates` module.

mod batch;
mod download;
mod pages;
mod settings;
mod translate;
mod upload;
mod viewer;

pub use batch::{start_translate_all, translate_all_stream};
pub use download::download_pdf;
pub use pages::{index, view_page, view_page_redirect};
pub use settings::{toggle_auto_translate, update_settings};
pub use translate::{prefetch_page, translate_page};
pub use upload::upload_pdf;
pub use viewer::{get_page_image, get_page_view, get_page_view_query, set_view_mode};

use serde::Deserialize as SerdeDeserialize;

/// Query params for page image.
#[derive(SerdeDeserialize, Default)]
pub struct PageImageQuery {
    #[serde(default)]
    pub translated: Option<String>,
}

/// Query params for page view (allows page input to use HTMX directly).
#[derive(SerdeDeserialize, Default)]
pub struct PageViewQuery {
    /// 1-based page number from the input field
    #[serde(default)]
    pub page: Option<usize>,
}

/// Form data for translation.
#[derive(SerdeDeserialize, Default)]
pub struct TranslateForm {
    /// Force re-translation, bypassing cache (Re-translate button sends force=1)
    #[serde(default)]
    pub force: Option<String>,
}

/// Settings update from form data.
#[derive(SerdeDeserialize)]
pub struct SettingsForm {
    pub source_lang: Option<String>,
    pub target_lang: Option<String>,
    pub text_color: Option<String>,
}

const PREFETCH_BACKWARD: usize = 1;
const PREFETCH_FORWARD: usize = 2;

/// Build Link header value for prefetching adjacent page images.
pub fn prefetch_links(session_id: &str, page: usize, page_count: usize) -> Option<String> {
    let start = page.saturating_sub(PREFETCH_BACKWARD);
    let end = (page + PREFETCH_FORWARD + 1).min(page_count);

    let links: Vec<_> = (start..end)
        .filter(|&p| p != page)
        .map(|p| format!("</api/page/{session_id}/{p}>; rel=prefetch; as=image"))
        .collect();

    (!links.is_empty()).then(|| links.join(", "))
}
