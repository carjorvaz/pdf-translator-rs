//! Translation routes - single page translation and prefetching.

use askama::Template;
use axum::{
    body::Body,
    extract::{Form, Path, State},
    http::{header, StatusCode},
    response::Response,
};
use std::sync::Arc;
use tracing::{debug, error};

use super::TranslateForm;
use crate::helpers::{validate_page, OptionExt, ResultExt, RouteResult};
use crate::state::AppState;
use crate::templates::TranslateResultTemplate;

/// Translate a single page - returns translated panel HTML.
///
/// HTMX: Replaces `#translated-content`, includes OOB toast and download button.
/// Page number is explicit in URL - semantically correct REST.
/// Re-translate sends `force=1` in POST body to bypass cache.
pub async fn translate_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
    Form(form): Form<TranslateForm>,
) -> RouteResult<Response> {
    let force_retranslate = form.force.is_some();
    // URL page is 1-based, convert to 0-based
    let page = url_page.saturating_sub(1);

    debug!(
        "translate_page: url_page={}, effective_page={}, force={}",
        url_page, page, force_retranslate
    );

    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (settings, doc, page_count) = session_ref
        .with_session(|s| {
            (
                s.settings.clone(),
                s.document.clone(), // O(1) clone - only clones Arc pointer
                s.document.page_count(),
            )
        })
        .await
        .or_not_found("Session not found")?;

    validate_page(page, page_count)?;

    // Update current page in session
    session_ref
        .with_session_mut(|s| s.current_page = page)
        .await;

    let translator = state.create_translator(&settings).map_err(|e| {
        error!("Failed to create translator: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    let template = match translator
        .translate_page_force(&doc, page, force_retranslate)
        .await
    {
        Ok(result) => {
            // Get path inside lock (fast)
            let path = session_ref
                .with_session(|s| s.page_store.page_path(page))
                .await
                .or_not_found("Session not found")?;

            // Write to disk outside lock (async)
            if let Err(e) = tokio::fs::write(&path, &result.pdf_bytes).await {
                error!("Failed to store translated page: {}", e);
            } else {
                // Mark stored inside lock (fast) — only if write succeeded
                session_ref
                    .with_session_mut(|s| s.page_store.mark_stored(page))
                    .await;
            }

            // Server-initiated prefetch: queue next 2 pages in background
            // This is cleaner than client-side hidden divs triggering requests
            for prefetch_page in [page + 1, page + 2] {
                if prefetch_page < page_count {
                    let state_clone = Arc::clone(&state);
                    let session_id_clone = session_id.clone();
                    tokio::spawn(async move {
                        prefetch_page_internal(&state_clone, &session_id_clone, prefetch_page).await;
                    });
                }
            }

            TranslateResultTemplate::success(session_id.clone(), page, result.from_cache)
        }
        Err(e) => {
            let error_msg = e.to_string();
            error!("Translation failed for page {}: {}", page, error_msg);
            TranslateResultTemplate::error(session_id.clone(), page, error_msg)
        }
    };

    let html = template.render().or_internal_error()?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .or_internal_error()
}

/// Internal prefetch logic - reused by both HTTP handler and server-initiated prefetch.
async fn prefetch_page_internal(
    state: &Arc<AppState>,
    session_id: &str,
    page: usize,
) {
    let Some(session_ref) = state.get_session(session_id).await else {
        return;
    };

    // Check if translation needed and claim the in-flight slot (inside lock)
    let data = session_ref
        .with_session_mut(|s| {
            if page >= s.document.page_count()
                || s.page_store.has_page(page)
                || s.in_flight.contains(&page)
            {
                debug!(
                    "Prefetch skipped for page {}: already done, in-flight, or out of range",
                    page
                );
                None // Out of range, already translated, or already in-flight
            } else {
                s.in_flight.insert(page);
                Some((
                    s.settings.clone(),
                    s.document.clone(),
                    s.page_store.page_path(page),
                ))
            }
        })
        .await;

    let Some((settings, doc, path)) = data.flatten() else {
        return; // Nothing to do
    };

    debug!("Prefetch starting translation for page {}", page);

    // Create translator and translate (outside lock - slow async ops)
    let Ok(translator) = state.create_translator(&settings) else {
        // Release in-flight claim on error
        session_ref.with_session_mut(|s| { s.in_flight.remove(&page); }).await;
        return;
    };

    let result = translator.translate_page_prefetch(&doc, page).await;

    // Release in-flight claim and store result
    match result {
        Ok(result) => {
            if tokio::fs::write(&path, &result.pdf_bytes).await.is_ok() {
                session_ref
                    .with_session_mut(|s| {
                        s.page_store.mark_stored(page);
                        s.in_flight.remove(&page);
                    })
                    .await;
                debug!("Prefetch completed for page {}", page);
            } else {
                session_ref.with_session_mut(|s| { s.in_flight.remove(&page); }).await;
            }
        }
        Err(_) => {
            session_ref.with_session_mut(|s| { s.in_flight.remove(&page); }).await;
        }
    }
}

/// Prefetch a page in background. Fire-and-forget: returns immediately.
///
/// Idempotent: skips if page already translated or out of range.
/// Can be called via HTTP for explicit prefetch requests.
pub async fn prefetch_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, page)): Path<(String, usize)>,
) -> StatusCode {
    debug!("Prefetch requested for page {} (0-based)", page);

    // Spawn background task - don't block the response
    tokio::spawn(async move {
        prefetch_page_internal(&state, &session_id, page).await;
    });

    StatusCode::NO_CONTENT
}
