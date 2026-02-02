//! Askama templates for HTMX responses.
//!
//! ## HTMX Patterns Used
//!
//! - **OOB Swaps**: Templates in `oob/` use `hx-swap-oob="true"` to update
//!   elements outside the main target (e.g., updating buttons after translation)
//!
//! - **Polling**: Progress template uses `hx-trigger="every 500ms"` for live updates
//!
//! - **Disabled Elements**: `hx-disabled-elt` prevents double-clicks during requests
//!
//! ## Template Structure
//!
//! - `base.html` - Common layout with CSS/JS
//! - `index.html` - Landing page with upload form
//! - `app.html` - Main app after PDF upload
//! - `partials/` - Reusable components (toolbar, viewer, pagination)
//! - `oob/` - Out-of-Band swap fragments

use askama::Template;
use askama_web::WebTemplate;
use pdf_translator_core::{
    LanguageOption, source_languages, target_languages, flag_for_lang,
    DEFAULT_SOURCE_LANG, DEFAULT_TARGET_LANG, DEFAULT_TEXT_COLOR,
};

// =============================================================================
// Full Page Templates
// =============================================================================

/// Landing page with upload form.
#[derive(Template, WebTemplate)]
#[template(path = "index.html")]
pub struct IndexTemplate;

/// Main app page after PDF upload.
///
/// Displays the dual-panel viewer with toolbar and controls.
#[derive(Template, WebTemplate)]
#[template(path = "app.html")]
pub struct AppTemplate {
    pub session_id: String,
    pub filename: String,
    pub page_count: usize,
    pub page: usize,
    pub has_translations: bool,
    pub is_translated: bool,
    /// Page version for cache-busting (content-based, not time-based)
    pub version: u64,
    // Language options from single source of truth
    pub source_languages: Vec<LanguageOption>,
    pub target_languages: Vec<LanguageOption>,
    pub default_source: &'static str,
    pub default_target: &'static str,
    pub default_color: &'static str,
}

impl AppTemplate {
    /// Create an app template for a specific page (used for direct URL access).
    pub fn at_page(
        session_id: String,
        filename: String,
        page_count: usize,
        page: usize,
        is_translated: bool,
        has_translations: bool,
        version: u64,
    ) -> Self {
        Self {
            session_id,
            filename,
            page_count,
            page,
            has_translations,
            is_translated,
            version,
            source_languages: source_languages(),
            target_languages: target_languages(),
            default_source: DEFAULT_SOURCE_LANG,
            default_target: DEFAULT_TARGET_LANG,
            default_color: DEFAULT_TEXT_COLOR,
        }
    }

    /// Previous page number (clamped to 0). Used in pagination template.
    pub const fn prev_page(&self) -> usize {
        if self.page > 0 {
            self.page - 1
        } else {
            0
        }
    }

    /// Next page number (clamped to last page). Used in pagination template.
    pub const fn next_page(&self) -> usize {
        if self.page + 1 < self.page_count {
            self.page + 1
        } else {
            self.page
        }
    }
}

// =============================================================================
// Fragment Templates (HTMX partial responses)
// =============================================================================

/// Viewer fragment returned when navigating pages.
///
/// Includes OOB updates for pagination and buttons.
#[derive(Template, WebTemplate)]
#[template(path = "partials/viewer_fragment.html")]
pub struct ViewerFragmentTemplate {
    pub session_id: String,
    pub page: usize,
    pub page_count: usize,
    pub is_translated: bool,
    pub has_any_translations: bool,
    /// Page version for cache-busting (content-based)
    pub version: u64,
}

impl ViewerFragmentTemplate {
    #[allow(clippy::missing_const_for_fn)] // String fields prevent const
    pub fn new(
        session_id: String,
        page: usize,
        page_count: usize,
        is_translated: bool,
        has_any_translations: bool,
        version: u64,
    ) -> Self {
        Self {
            session_id,
            page,
            page_count,
            is_translated,
            has_any_translations,
            version,
        }
    }

    /// Previous page number (clamped to 0). Used in pagination template.
    pub const fn prev_page(&self) -> usize {
        if self.page > 0 {
            self.page - 1
        } else {
            0
        }
    }

    /// Next page number (clamped to last page). Used in pagination template.
    pub const fn next_page(&self) -> usize {
        if self.page + 1 < self.page_count {
            self.page + 1
        } else {
            self.page
        }
    }
}

/// Translated panel content after successful translation.
///
/// Also used for error display when `is_error` is true.
/// When `page_changed` is true, includes OOB updates for original image and pagination.
#[derive(Template, WebTemplate)]
#[template(path = "partials/translate_result.html")]
pub struct TranslateResultTemplate {
    pub session_id: String,
    pub page: usize,
    pub page_count: usize,
    pub page_changed: bool,
    /// Page version for cache-busting (content-based)
    pub version: u64,
    pub is_error: bool,
    /// Used in included template `oob/translate_btn.html` via Askama context
    #[allow(dead_code)]
    pub is_translated: bool,
    pub message: String,
}

impl TranslateResultTemplate {
    pub fn success(
        session_id: String,
        page: usize,
        page_count: usize,
        page_changed: bool,
        from_cache: bool,
        version: u64,
    ) -> Self {
        Self {
            session_id,
            page,
            page_count,
            page_changed,
            version,
            is_error: false,
            is_translated: true, // After successful translation, page is translated
            message: if from_cache {
                "Loaded from cache".to_string()
            } else {
                "Translation complete".to_string()
            },
        }
    }

    #[allow(clippy::missing_const_for_fn)] // String fields prevent const
    pub fn error(
        session_id: String,
        page: usize,
        page_count: usize,
        page_changed: bool,
        error: String,
    ) -> Self {
        Self {
            session_id,
            page,
            page_count,
            page_changed,
            version: 0,
            is_error: true,
            is_translated: false, // Don't change button state on error
            message: error,
        }
    }

    /// Helper for pagination prev button (0-based)
    pub const fn prev_page(&self) -> usize {
        self.page.saturating_sub(1)
    }

    /// Helper for pagination next button (0-based)
    pub fn next_page(&self) -> usize {
        (self.page + 1).min(self.page_count.saturating_sub(1))
    }
}

/// Settings cleared fragment - shows placeholder after settings change.
///
/// Includes OOB swaps for flag and swatch indicators so the server
/// controls all UI state (no client-side JS needed for visual updates).
#[derive(Template, WebTemplate)]
#[template(path = "partials/settings_cleared.html")]
pub struct SettingsClearedTemplate {
    pub source_flag: &'static str,
    pub target_flag: &'static str,
    pub text_color: &'static str,
}

impl SettingsClearedTemplate {
    pub fn new(source_lang: &str, target_lang: &str, text_color: &str) -> Self {
        Self {
            source_flag: flag_for_lang(source_lang),
            target_flag: flag_for_lang(target_lang),
            text_color: color_class(text_color),
        }
    }
}

impl Default for SettingsClearedTemplate {
    fn default() -> Self {
        Self {
            source_flag: flag_for_lang(DEFAULT_SOURCE_LANG),
            target_flag: flag_for_lang(DEFAULT_TARGET_LANG),
            text_color: color_class(DEFAULT_TEXT_COLOR),
        }
    }
}

/// Get CSS class for a color name
#[allow(clippy::match_same_arms)] // Explicit "blue" arm for clarity, default also returns blue
fn color_class(name: &str) -> &'static str {
    match name.to_lowercase().as_str() {
        "darkred" | "dark_red" | "dark-red" => "darkred",
        "black" => "black",
        "blue" => "blue",
        "darkgreen" | "dark_green" | "dark-green" => "darkgreen",
        "purple" => "purple",
        _ => "blue",
    }
}

/// Progress bar for translate-all operation.
///
/// When `done` is false, includes HTMX polling trigger (will be replaced with SSE).
/// When `done` is true, includes completion toast and button re-enabling.
#[derive(Template, WebTemplate)]
#[template(path = "partials/progress.html")]
pub struct ProgressTemplate {
    pub session_id: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
    pub done: bool,
    pub current_page: usize,
    pub has_error: bool,
    /// Page version for the current page (for cache-busting translated image)
    pub version: u64,
}

impl ProgressTemplate {
    #[allow(clippy::too_many_arguments)] // All fields needed for template rendering
    #[allow(clippy::missing_const_for_fn)] // String fields prevent const
    pub fn new(
        session_id: String,
        current: usize,
        total: usize,
        message: String,
        done: bool,
        current_page: usize,
        has_error: bool,
        version: u64,
    ) -> Self {
        Self {
            session_id,
            current,
            total,
            message,
            done,
            current_page,
            has_error,
            version,
        }
    }

    /// Percentage complete (0-100).
    pub const fn percent(&self) -> usize {
        if self.total > 0 {
            (self.current * 100) / self.total
        } else {
            0
        }
    }

    /// Whether to show translated content OOB update.
    pub const fn show_translated_content(&self) -> bool {
        self.done && !self.has_error && self.current > self.current_page
    }

    /// Toast type based on state.
    pub const fn toast_type(&self) -> &'static str {
        if self.has_error {
            "error"
        } else {
            "success"
        }
    }

    /// Toast message based on state.
    pub fn toast_message(&self) -> String {
        if self.has_error {
            self.message.clone()
        } else {
            format!("Translated {} pages", self.current)
        }
    }
}
