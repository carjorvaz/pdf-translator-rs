//! Page routes - full HTML page renders.

use super::{ViewPageQuery, page_index};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use std::sync::Arc;

use crate::helpers::{OptionExt, RouteResult, validate_page};
use crate::state::{AppState, ViewMode};
use crate::templates::{AppTemplate, IndexTemplate};

/// Landing page with upload form.
pub async fn index() -> IndexTemplate {
    IndexTemplate
}

/// Redirect /view/{session_id} to /view/{session_id}/1 (canonical URL).
///
/// Ensures consistent URLs - page 1 is explicit, not implicit.
pub async fn view_page_redirect(
    Path(session_id): Path<String>,
    Query(query): Query<ViewPageQuery>,
) -> RouteResult<Redirect> {
    let suffix = match query.mode.as_deref() {
        Some("both") => "?mode=both",
        Some("translated") => "?mode=translated",
        Some(_) => return Err((StatusCode::BAD_REQUEST, "Invalid view mode".to_string())),
        None => "",
    };
    Ok(Redirect::permanent(&format!(
        "/view/{session_id}/1{suffix}"
    )))
}

/// View a specific page (for direct URL access and browser history).
///
/// Returns the full app page at the specified page number.
/// URL uses 1-based page numbers for better UX (page 1 = first page).
pub async fn view_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
    Query(query): Query<ViewPageQuery>,
) -> RouteResult<AppTemplate> {
    let page = page_index(url_page)?;

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (
        filename,
        page_count,
        is_translated,
        has_translations,
        current_source,
        current_target,
        current_color,
        default_view_mode,
        auto_translate,
    ) = session
        .with_session(|s| {
            (
                s.original_filename.clone(),
                s.document.page_count(),
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.settings.current_source().to_string(),
                s.settings.current_target().to_string(),
                s.settings.current_color().to_string(),
                s.settings.view_mode,
                s.settings.auto_translate,
            )
        })
        .await
        .or_not_found("Session not found")?;

    validate_page(page, page_count)?;
    let view_mode = match query.mode.as_deref() {
        Some("both") => ViewMode::Both,
        Some("translated") => ViewMode::TranslatedOnly,
        Some(_) => return Err((StatusCode::BAD_REQUEST, "Invalid view mode".to_string())),
        None => default_view_mode,
    };

    Ok(AppTemplate::at_page(
        session_id,
        filename,
        page_count,
        page,
        is_translated,
        has_translations,
        current_source,
        current_target,
        current_color,
        view_mode,
        auto_translate,
    ))
}
