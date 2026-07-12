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
use pdf_translator_core::{LanguageOption, source_languages, target_languages};

use crate::state::ViewMode;

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
#[allow(clippy::struct_excessive_bools)] // Flat Askama context mirrors independent controls.
pub struct AppTemplate {
    pub session_id: String,
    pub filename: String,
    pub page_count: usize,
    pub page: usize,
    pub has_translations: bool,
    pub is_translated: bool,
    // Language options from single source of truth
    pub source_languages: Vec<LanguageOption>,
    pub target_languages: Vec<LanguageOption>,
    pub current_source: String,
    pub current_target: String,
    pub current_color: String,
    /// CSS class for the viewer div (derived from view_mode)
    pub viewer_class: &'static str,
    /// Whether showing translated-only view (derived from view_mode)
    pub view_translated_only: bool,
    /// Stable query value for request-local presentation mode.
    pub view_mode: &'static str,
    /// Whether this is an OOB response (for pagination consolidation)
    #[allow(dead_code)]
    pub is_oob: bool,
    /// Auto-translate on navigation
    pub auto_translate: bool,
}

impl AppTemplate {
    /// Create an app template for a specific page (used for direct URL access).
    #[allow(clippy::too_many_arguments)] // Explicit server-rendered page state.
    pub fn at_page(
        session_id: String,
        filename: String,
        page_count: usize,
        page: usize,
        is_translated: bool,
        has_translations: bool,
        current_source: String,
        current_target: String,
        current_color: String,
        view_mode: ViewMode,
        auto_translate: bool,
    ) -> Self {
        Self {
            session_id,
            filename,
            page_count,
            page,
            has_translations,
            is_translated,
            source_languages: source_languages(),
            target_languages: target_languages(),
            current_source,
            current_target,
            current_color,
            viewer_class: view_mode.viewer_class(),
            view_translated_only: view_mode.is_translated_only(),
            view_mode: if view_mode.is_translated_only() {
                "translated"
            } else {
                "both"
            },
            is_oob: false, // Initial page render doesn't use OOB
            auto_translate,
        }
    }

    /// Previous page number (clamped to 0). Used in pagination template.
    pub const fn prev_page(&self) -> usize {
        if self.page > 0 { self.page - 1 } else { 0 }
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
#[allow(clippy::struct_excessive_bools)] // Flat Askama context mirrors independent controls.
pub struct ViewerFragmentTemplate {
    pub session_id: String,
    pub page: usize,
    pub page_count: usize,
    pub is_translated: bool,
    pub has_any_translations: bool,
    /// CSS class for the viewer div (derived from view_mode)
    pub viewer_class: &'static str,
    /// Whether showing translated-only view (derived from view_mode).
    pub view_translated_only: bool,
    /// Stable query value for request-local presentation mode.
    pub view_mode: &'static str,
    /// Whether this is an OOB response (for pagination consolidation)
    pub is_oob: bool,
    /// Auto-translate on navigation
    pub auto_translate: bool,
}

impl ViewerFragmentTemplate {
    #[allow(clippy::missing_const_for_fn)] // String fields prevent const
    pub fn new(
        session_id: String,
        page: usize,
        page_count: usize,
        is_translated: bool,
        has_any_translations: bool,
        view_mode: ViewMode,
        auto_translate: bool,
    ) -> Self {
        Self {
            session_id,
            page,
            page_count,
            is_translated,
            has_any_translations,
            viewer_class: view_mode.viewer_class(),
            view_translated_only: view_mode.is_translated_only(),
            view_mode: if view_mode.is_translated_only() {
                "translated"
            } else {
                "both"
            },
            is_oob: true, // Fragment responses need OOB swaps
            auto_translate,
        }
    }

    /// Previous page number (clamped to 0). Used in pagination template.
    pub const fn prev_page(&self) -> usize {
        if self.page > 0 { self.page - 1 } else { 0 }
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
/// Page is explicit in URL - no page_changed inference needed.
#[derive(Template, WebTemplate)]
#[template(path = "partials/translate_result.html")]
pub struct TranslateResultTemplate {
    pub session_id: String,
    pub page: usize,
    pub is_error: bool,
    /// Used in included template `oob/translate_btn.html` via Askama context
    #[allow(dead_code)]
    pub is_translated: bool,
    pub version: u64,
    pub message: String,
}

impl TranslateResultTemplate {
    pub fn success(session_id: String, page: usize, version: u64, from_cache: bool) -> Self {
        Self {
            session_id,
            page,
            is_error: false,
            is_translated: true,
            version,
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
        is_translated: bool,
        version: u64,
        error: String,
    ) -> Self {
        Self {
            session_id,
            page,
            is_error: true,
            is_translated,
            version,
            message: error,
        }
    }
}

/// Settings cleared fragment - shows placeholder after settings change.
#[derive(Template, WebTemplate, Default)]
#[template(path = "partials/settings_cleared.html")]
pub struct SettingsClearedTemplate;

/// Auto-translate toggle response - returns OOB checkbox update.
#[derive(Template, WebTemplate)]
#[template(path = "partials/auto_translate_toggle.html")]
pub struct AutoTranslateToggleTemplate {
    pub session_id: String,
    pub auto_translate: bool,
}

/// Progress bar for translate-all operation.
///
/// The initial response owns the SSE connection; event responses replace only
/// its stable child so the connection is not repeatedly recreated.
#[derive(Template, WebTemplate)]
#[template(path = "partials/progress.html")]
pub struct ProgressTemplate {
    pub session_id: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
    pub done: bool,
    pub has_error: bool,
    pub connect_sse: bool,
}

impl ProgressTemplate {
    #[allow(clippy::missing_const_for_fn)] // String fields prevent const
    pub fn new(
        session_id: String,
        current: usize,
        total: usize,
        message: String,
        done: bool,
        has_error: bool,
        connect_sse: bool,
    ) -> Self {
        Self {
            session_id,
            current,
            total,
            message,
            done,
            has_error,
            connect_sse,
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

    /// Toast type based on state.
    pub const fn toast_type(&self) -> &'static str {
        if self.has_error { "error" } else { "success" }
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

/// View mode toggle response - returns viewer with new class and OOB updates for toggle buttons.
#[derive(Template, WebTemplate)]
#[template(path = "partials/view_mode.html")]
#[allow(clippy::struct_excessive_bools)] // Flat Askama context mirrors independent controls.
pub struct ViewModeTemplate {
    pub session_id: String,
    pub page: usize,
    pub page_count: usize,
    pub is_translated: bool,
    pub auto_translate: bool,
    /// CSS class for the viewer div (derived from view_mode)
    pub viewer_class: &'static str,
    /// Whether showing translated-only view (derived from view_mode)
    pub view_translated_only: bool,
    /// Stable query value for request-local presentation mode.
    pub view_mode: &'static str,
    /// Pagination is emitted as an out-of-band refresh.
    pub is_oob: bool,
}

impl ViewModeTemplate {
    pub const fn new(
        session_id: String,
        page: usize,
        page_count: usize,
        is_translated: bool,
        auto_translate: bool,
        view_mode: ViewMode,
    ) -> Self {
        Self {
            session_id,
            page,
            page_count,
            is_translated,
            auto_translate,
            viewer_class: view_mode.viewer_class(),
            view_translated_only: view_mode.is_translated_only(),
            view_mode: if view_mode.is_translated_only() {
                "translated"
            } else {
                "both"
            },
            is_oob: true,
        }
    }

    pub const fn prev_page(&self) -> usize {
        if self.page > 0 { self.page - 1 } else { 0 }
    }

    pub const fn next_page(&self) -> usize {
        if self.page + 1 < self.page_count {
            self.page + 1
        } else {
            self.page
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn view_mode_response_preserves_mode_navigation_and_auto_translate() {
        let html = ViewModeTemplate::new(
            "session".to_string(),
            1,
            3,
            false,
            true,
            ViewMode::TranslatedOnly,
        )
        .render()
        .unwrap();

        assert!(html.contains("?mode=translated"));
        assert!(html.contains("hx-vals='{\"mode\":\"translated\"}'"));
        assert!(html.contains("hx-trigger=\"load\""));
        assert!(html.contains("hx-sync=\"#app:abort\""));
        assert!(html.contains("id=\"pagination\" hx-swap-oob=\"true\""));
    }

    #[test]
    fn progress_response_owns_and_closes_terminal_sse() {
        let active = ProgressTemplate::new(
            "session".to_string(),
            1,
            2,
            "Working".to_string(),
            false,
            false,
            true,
        )
        .render()
        .unwrap();
        assert!(active.contains("sse-swap=\"progress,complete\""));
        assert!(active.contains("sse-close=\"complete\""));

        let done = ProgressTemplate::new(
            "session".to_string(),
            2,
            2,
            "Done".to_string(),
            true,
            false,
            false,
        )
        .render()
        .unwrap();
        assert!(done.contains("add .auto-hide to #progress-area"));
        assert!(done.contains("hx-boost=\"false\""));
    }

    #[test]
    fn translation_result_versions_images_and_preserves_failed_retry_state() {
        let success = TranslateResultTemplate::success("session".to_string(), 0, 7, false)
            .render()
            .unwrap();
        assert!(success.contains("translated=1&amp;version=7"));
        assert!(success.contains("hx-boost=\"false\""));

        let error =
            TranslateResultTemplate::error("session".to_string(), 0, true, 7, "<bad>".to_string())
                .render()
                .unwrap();
        assert!(error.contains("translated=1&amp;version=7"));
        assert!(error.contains("force"));
        assert!(error.contains("bad"));
        assert!(!error.contains("<bad>"));
    }
}
