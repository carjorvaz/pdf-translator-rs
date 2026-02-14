//! Viewer routes - page viewing and image rendering.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use pdf_translator_core::{render_page, render_page_webp, PdfDocument};
use std::sync::Arc;

use super::{prefetch_links, PageImageQuery, PageViewQuery};
use crate::helpers::{validate_page, OptionExt, ResultExt, RouteResult};
use crate::state::{AppState, ViewMode};
use crate::templates::{ViewerFragmentTemplate, ViewModeTemplate};

/// Get page image as PNG or WebP (based on Accept header).
///
/// Supports ETag-based caching for translated pages. The ETag is based on
/// the page version, which increments each time the page is re-translated.
pub async fn get_page_image(
    State(state): State<Arc<AppState>>,
    Path((session_id, page)): Path<(String, usize)>,
    Query(query): Query<PageImageQuery>,
    headers: HeaderMap,
) -> RouteResult<Response> {
    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let page_count = session
        .with_session(|s| s.document.page_count())
        .await
        .or_not_found("Session not found")?;
    validate_page(page, page_count)?;

    // Check if browser supports WebP
    let use_webp = headers
        .get(header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| s.contains("image/webp"));
    let content_type = if use_webp { "image/webp" } else { "image/png" };

    // DPR-aware rendering via Client Hints (integer scaling only: 1x or 2x)
    let scale = headers
        .get("dpr")
        .or_else(|| headers.get("sec-ch-dpr"))
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<f32>().ok())
        .map_or(2.0, |dpr| if dpr < 1.5 { 1.0 } else { 2.0 });

    let wants_translated = query.translated.is_some();

    // For translated pages, check ETag for cache validation
    if wants_translated {
        // Get metadata inside lock (fast)
        let (has_page, version, path) = session
            .with_session(|s| {
                (
                    s.page_store.has_page(page),
                    s.page_store.version(page),
                    s.page_store.page_path(page),
                )
            })
            .await
            .or_not_found("Session not found")?;

        if has_page {
            // Include format and scale in ETag so variants are cached separately
            let format_tag = if use_webp { "webp" } else { "png" };
            let etag = format!("\"{session_id}-{page}-{version}-{format_tag}-{scale}\"");

            // Check If-None-Match header for 304 response
            if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH)
                && if_none_match.to_str().ok() == Some(etag.as_str())
            {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .body(Body::empty())
                    .or_internal_error();
            }

            // Load translated PDF from disk (async, outside lock)
            let pdf_bytes = tokio::fs::read(&path).await.or_internal_error()?;

            // Parse and render in blocking task to avoid blocking async runtime
            // Translated PDFs are single-page (via keep_single_page), so always render page 0
            let image_data = tokio::task::spawn_blocking(move || {
                let doc = PdfDocument::from_bytes(pdf_bytes)?;
                if use_webp {
                    render_page_webp(&doc, 0, scale)
                } else {
                    render_page(&doc, 0, scale)
                }
            })
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Render task panicked: {e}"),
                )
            })?
            .or_internal_error()?;

            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ETAG, etag)
                .header(
                    header::CACHE_CONTROL,
                    "private, max-age=3600, must-revalidate",
                )
                .header("Accept-CH", "DPR")
                .header(header::VARY, "DPR")
                .body(Body::from(image_data))
                .or_internal_error();
        }

        // Fall through to render original if no translation
    }

    // Render original page - can be cached aggressively (never changes within session)
    // Include format and scale in ETag so variants are cached separately
    let format_tag = if use_webp { "webp" } else { "png" };
    let etag = format!("\"orig-{session_id}-{page}-{format_tag}-{scale}\"");

    // Check If-None-Match for 304 response
    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH)
        && if_none_match.to_str().ok() == Some(etag.as_str())
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .or_internal_error();
    }

    // Clone document inside lock (O(1) - only clones Arc pointer)
    let doc = session
        .with_session(|s| s.document.clone())
        .await
        .or_not_found("Session not found")?;

    // Render in blocking task to avoid blocking async runtime
    let image_data = tokio::task::spawn_blocking(move || {
        if use_webp {
            render_page_webp(&doc, page, scale)
        } else {
            render_page(&doc, page, scale)
        }
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Render task panicked: {e}"),
        )
    })?
    .or_internal_error()?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, max-age=3600, immutable")
        .header("Accept-CH", "DPR")
        .header(header::VARY, "DPR")
        .body(Body::from(image_data))
        .or_internal_error()
}

/// Get page view - returns viewer HTML fragment with OOB updates.
///
/// HTMX: Replaces `#viewer`, includes OOB updates for pagination and buttons.
/// URL uses 1-based page numbers for better UX (page 1 = first page).
/// Includes Link headers for prefetching adjacent page images.
pub async fn get_page_view(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
) -> RouteResult<Response> {
    // Convert 1-based URL page to 0-based internal page
    let page = url_page.saturating_sub(1);

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Update current page in session
    session.with_session_mut(|s| s.current_page = page).await;

    let (is_translated, has_any_translations, page_count, view_mode, auto_translate) = session
        .with_session(|s| {
            (
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.document.page_count(),
                s.settings.view_mode,
                s.settings.auto_translate,
            )
        })
        .await
        .or_not_found("Session not found")?;

    validate_page(page, page_count)?;

    let template = ViewerFragmentTemplate::new(
        session_id.clone(),
        page,
        page_count,
        is_translated,
        has_any_translations,
        view_mode,
        auto_translate,
    );

    let html = template.render().or_internal_error()?;
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8");

    // Only include Link header if there are pages to prefetch
    if let Some(links) = prefetch_links(&session_id, page, page_count) {
        builder = builder.header(header::LINK, links);
    }

    builder.body(Body::from(html)).or_internal_error()
}

/// Get page view via query parameter - for HTMX page input.
///
/// Accepts `?page=N` (1-based) instead of path parameter.
/// This allows the page input to be a proper hypermedia control.
/// Returns HX-Push-Url header so browser URL updates without JavaScript.
pub async fn get_page_view_query(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<PageViewQuery>,
) -> RouteResult<Response> {
    // Default to page 1 if not specified, convert to 0-based
    let url_page = query.page.unwrap_or(1);
    let page = url_page.saturating_sub(1);

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Update current page in session
    session.with_session_mut(|s| s.current_page = page).await;

    let (is_translated, has_any_translations, page_count, view_mode, auto_translate) = session
        .with_session(|s| {
            (
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.document.page_count(),
                s.settings.view_mode,
                s.settings.auto_translate,
            )
        })
        .await
        .or_not_found("Session not found")?;

    validate_page(page, page_count)?;

    let template = ViewerFragmentTemplate::new(
        session_id.clone(),
        page,
        page_count,
        is_translated,
        has_any_translations,
        view_mode,
        auto_translate,
    );

    let html = template.render().or_internal_error()?;
    let push_url = format!("/view/{session_id}/{url_page}");

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("hx-push-url", push_url);

    // Only include Link header if there are pages to prefetch
    if let Some(links) = prefetch_links(&session_id, page, page_count) {
        builder = builder.header(header::LINK, links);
    }

    builder.body(Body::from(html)).or_internal_error()
}

/// Toggle view mode (Both or Translated Only) - returns viewer with new state.
///
/// HTMX: Replaces `#viewer`, includes OOB swap for view toggle buttons.
/// This keeps view state server-controlled instead of client-side JS.
pub async fn set_view_mode(
    State(state): State<Arc<AppState>>,
    Path((session_id, mode)): Path<(String, String)>,
) -> RouteResult<ViewModeTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Parse mode and update session
    let new_mode = match mode.as_str() {
        "both" => ViewMode::Both,
        "translated" => ViewMode::TranslatedOnly,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid view mode".to_string())),
    };

    let (page, is_translated) = session_ref
        .with_session_mut(|s| {
            s.settings.view_mode = new_mode;
            (s.current_page, s.page_store.has_page(s.current_page))
        })
        .await
        .or_not_found("Session not found")?;

    Ok(ViewModeTemplate::new(
        session_id,
        page,
        is_translated,
        new_mode,
    ))
}
