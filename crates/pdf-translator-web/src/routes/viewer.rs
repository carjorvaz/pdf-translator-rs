//! Viewer routes - page viewing and image rendering.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use pdf_translator_core::{PdfDocument, render_page, render_page_webp};
use std::sync::Arc;

use super::{PageImageQuery, PageViewQuery, page_index, prefetch_links};
use crate::helpers::{OptionExt, ResultExt, RouteResult, validate_page};
use crate::state::{AppState, ViewMode};
use crate::templates::{ViewModeTemplate, ViewerFragmentTemplate};

const IMAGE_VARY: &str = "Accept, DPR, Sec-CH-DPR";

fn requested_view_mode(mode: Option<&str>, default: ViewMode) -> RouteResult<ViewMode> {
    match mode {
        Some("both") => Ok(ViewMode::Both),
        Some("translated") => Ok(ViewMode::TranslatedOnly),
        Some(_) => Err((StatusCode::BAD_REQUEST, "Invalid view mode".to_string())),
        None => Ok(default),
    }
}

fn accepts_webp(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|range| {
            let mut parts = range.split(';');
            let is_webp = parts
                .next()
                .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("image/webp"));
            if !is_webp {
                return false;
            }

            parts
                .filter_map(|parameter| parameter.split_once('='))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
                .is_none_or(|(_, value)| {
                    value
                        .trim()
                        .parse::<f32>()
                        .is_ok_and(|quality| quality > 0.0)
                })
        })
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
        })
}

fn image_response(status: StatusCode) -> axum::http::response::Builder {
    Response::builder()
        .status(status)
        .header("Accept-CH", "DPR")
        .header(header::VARY, IMAGE_VARY)
}

fn translated_not_ready() -> RouteResult<Response> {
    image_response(StatusCode::NOT_FOUND)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Translated page not ready"))
        .or_internal_error()
}

fn translated_changed() -> RouteResult<Response> {
    image_response(StatusCode::CONFLICT)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Translated page changed; retry request"))
        .or_internal_error()
}

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

    let use_webp = accepts_webp(&headers);
    let content_type = if use_webp { "image/webp" } else { "image/png" };
    let format_tag = if use_webp { "webp" } else { "png" };

    // DPR-aware rendering via Client Hints (integer scaling only: 1x or 2x).
    let scale = headers
        .get("dpr")
        .or_else(|| headers.get("sec-ch-dpr"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f32>().ok())
        .map_or(2.0, |dpr| if dpr < 1.5 { 1.0 } else { 2.0 });

    if query.translated.is_some() {
        // A translation can be replaced while its immutable snapshot is being
        // rendered. Retry once against the new version rather than publishing
        // pixels whose ETag no longer describes the current page.
        for _ in 0..2 {
            let snapshot = session
                .with_session(|s| s.page_store.page_snapshot(page))
                .await
                .or_not_found("Session not found")?;
            let Some(snapshot) = snapshot else {
                return translated_not_ready();
            };

            let etag = format!(
                "\"{session_id}-{page}-{}-{format_tag}-{scale}\"",
                snapshot.version()
            );
            if if_none_match(&headers, &etag) {
                let unchanged = session
                    .with_session(|s| {
                        s.page_store
                            .page_snapshot(page)
                            .is_some_and(|current| Arc::ptr_eq(&current, &snapshot))
                    })
                    .await
                    .or_not_found("Session not found")?;
                if unchanged {
                    return image_response(StatusCode::NOT_MODIFIED)
                        .header(header::ETAG, etag)
                        .header(
                            header::CACHE_CONTROL,
                            "private, max-age=3600, must-revalidate",
                        )
                        .body(Body::empty())
                        .or_internal_error();
                }
                continue;
            }

            let pdf_bytes = match tokio::fs::read(snapshot.path()).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return translated_not_ready();
                }
                Err(error) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
            };

            // Translated PDFs contain one page. Parsing and rendering remain
            // off the async executor because both are CPU-bound.
            let image_data = tokio::task::spawn_blocking(move || {
                let doc = PdfDocument::from_bytes(pdf_bytes)?;
                if use_webp {
                    render_page_webp(&doc, 0, scale)
                } else {
                    render_page(&doc, 0, scale)
                }
            })
            .await
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Render task panicked: {error}"),
                )
            })?
            .or_internal_error()?;

            let unchanged = session
                .with_session(|s| {
                    s.page_store
                        .page_snapshot(page)
                        .is_some_and(|current| Arc::ptr_eq(&current, &snapshot))
                })
                .await
                .or_not_found("Session not found")?;
            if !unchanged {
                continue;
            }

            return image_response(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ETAG, etag)
                .header(
                    header::CACHE_CONTROL,
                    "private, max-age=3600, must-revalidate",
                )
                .body(Body::from(image_data))
                .or_internal_error();
        }

        return translated_changed();
    }

    // Original pages are immutable for the lifetime of a session.
    let etag = format!("\"orig-{session_id}-{page}-{format_tag}-{scale}\"");
    if if_none_match(&headers, &etag) {
        return image_response(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "private, max-age=3600, immutable")
            .body(Body::empty())
            .or_internal_error();
    }

    let doc = session
        .with_session(|s| s.document.clone())
        .await
        .or_not_found("Session not found")?;
    let image_data = tokio::task::spawn_blocking(move || {
        if use_webp {
            render_page_webp(&doc, page, scale)
        } else {
            render_page(&doc, page, scale)
        }
    })
    .await
    .map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Render task panicked: {error}"),
        )
    })?
    .or_internal_error()?;

    image_response(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, max-age=3600, immutable")
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
    Query(query): Query<PageViewQuery>,
) -> RouteResult<Response> {
    let page = page_index(url_page)?;

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (is_translated, has_any_translations, page_count, default_view_mode, auto_translate) =
        session
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
    let view_mode = requested_view_mode(query.mode.as_deref(), default_view_mode)?;

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
    let url_page = query.page.unwrap_or(1);
    let page = page_index(url_page)?;

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (is_translated, has_any_translations, page_count, default_view_mode, auto_translate) =
        session
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
    let view_mode = requested_view_mode(query.mode.as_deref(), default_view_mode)?;

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
    let push_url = format!(
        "/view/{session_id}/{url_page}?mode={}",
        if view_mode.is_translated_only() {
            "translated"
        } else {
            "both"
        }
    );

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
    Path((session_id, url_page, mode)): Path<(String, usize, String)>,
) -> RouteResult<ViewModeTemplate> {
    let page = page_index(url_page)?;
    let new_mode = match mode.as_str() {
        "both" => ViewMode::Both,
        "translated" => ViewMode::TranslatedOnly,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid view mode".to_string())),
    };

    let session = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;
    let (page_count, is_translated, auto_translate) = session
        .with_session(|s| {
            (
                s.document.page_count(),
                s.page_store.has_page(page),
                s.settings.auto_translate,
            )
        })
        .await
        .or_not_found("Session not found")?;
    validate_page(page, page_count)?;

    // View mode is request-local: one browser tab cannot change another tab's
    // presentation. The explicit page keeps this response independent of any
    // navigation request racing in the same session.
    Ok(ViewModeTemplate::new(
        session_id,
        page,
        page_count,
        is_translated,
        auto_translate,
        new_mode,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn view_mode_query_is_strict_and_preserves_default() {
        assert_eq!(
            requested_view_mode(None, ViewMode::TranslatedOnly).unwrap(),
            ViewMode::TranslatedOnly
        );
        assert_eq!(
            requested_view_mode(Some("both"), ViewMode::TranslatedOnly).unwrap(),
            ViewMode::Both
        );
        assert_eq!(
            requested_view_mode(Some("translated"), ViewMode::Both).unwrap(),
            ViewMode::TranslatedOnly
        );
        assert_eq!(
            requested_view_mode(Some("Translated"), ViewMode::Both)
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn webp_negotiation_rejects_zero_quality() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("image/avif, image/webp;q=0, image/png;q=1"),
        );
        assert!(!accepts_webp(&headers));

        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("image/png;q=0.8, image/webp;q=0.5"),
        );
        assert!(accepts_webp(&headers));
    }

    #[test]
    fn etag_matching_handles_lists_weak_tags_and_wildcards() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"old\", W/\"current\""),
        );
        assert!(if_none_match(&headers, "\"current\""));
        assert!(!if_none_match(&headers, "\"other\""));

        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(if_none_match(&headers, "\"anything\""));
    }

    #[test]
    fn public_page_numbers_never_underflow() {
        assert_eq!(page_index(1).unwrap(), 0);
        assert_eq!(page_index(2).unwrap(), 1);
        assert_eq!(page_index(0).unwrap_err().0, StatusCode::BAD_REQUEST);
    }
}
