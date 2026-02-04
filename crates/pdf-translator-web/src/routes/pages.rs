//! Page routes - full HTML page renders.

use axum::extract::{Path, State};
use axum::response::Redirect;
use std::sync::Arc;

use crate::helpers::{validate_page, OptionExt, RouteResult};
use crate::state::AppState;
use crate::templates::{AppTemplate, IndexTemplate};

/// Landing page with upload form.
pub async fn index() -> IndexTemplate {
    IndexTemplate
}

/// Redirect /view/{session_id} to /view/{session_id}/1 (canonical URL).
///
/// Ensures consistent URLs - page 1 is explicit, not implicit.
pub async fn view_page_redirect(Path(session_id): Path<String>) -> Redirect {
    Redirect::permanent(&format!("/view/{session_id}/1"))
}

/// View a specific page (for direct URL access and browser history).
///
/// Returns the full app page at the specified page number.
/// URL uses 1-based page numbers for better UX (page 1 = first page).
pub async fn view_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
) -> RouteResult<AppTemplate> {
    // Convert 1-based URL page to 0-based internal page
    let page = url_page.saturating_sub(1);

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Update current page in session
    session.with_session_mut(|s| s.current_page = page).await;

    let (filename, page_count, is_translated, has_translations, view_mode, auto_translate) = session
        .with_session(|s| {
            (
                s.original_filename.clone(),
                s.document.page_count(),
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.settings.view_mode,
                s.settings.auto_translate,
            )
        })
        .await
        .or_not_found("Session not found")?;

    validate_page(page, page_count)?;

    Ok(AppTemplate::at_page(
        session_id,
        filename,
        page_count,
        page,
        is_translated,
        has_translations,
        view_mode,
        auto_translate,
    ))
}
