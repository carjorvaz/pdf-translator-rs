//! HTTP route handlers for the PDF translator web application.
//!
//! All routes return either HTML (for HTMX consumption) or binary data (images, PDFs).
//! HTML routes use Askama templates from the `templates` module.

use askama::Template;
use axum::{
    body::Body,
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Response,
    },
};
use axum_extra::extract::Multipart;
use futures::stream::Stream;
use pdf_translator_core::{render_page, render_page_webp, Lang, PdfDocument, TextColor};
use serde::Deserialize as SerdeDeserialize;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::helpers::{validate_page, OptionExt, ResultExt, RouteResult};
use crate::state::{AppState, TranslateJob};
use crate::templates::{
    AppTemplate, IndexTemplate, ProgressTemplate, SettingsClearedTemplate,
    TranslateResultTemplate, ViewerFragmentTemplate,
};

// =============================================================================
// Page Routes
// =============================================================================

/// Landing page with upload form.
pub async fn index() -> IndexTemplate {
    IndexTemplate
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

    let (filename, page_count, is_translated, has_translations, version) = session
        .with_session(|s| {
            (
                s.original_filename.clone(),
                s.document.page_count(),
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.page_store.version(page),
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
        version,
    ))
}

/// Upload a PDF file - redirects to view page (POST-Redirect-GET pattern).
pub async fn upload_pdf(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> RouteResult<Response> {
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let filename = field.file_name().unwrap_or("document.pdf").to_string();

            let data = field.bytes().await.or_bad_request()?;

            // Parse PDF in a blocking task to avoid blocking the async runtime
            let data_vec = data.to_vec();
            let doc = tokio::task::spawn_blocking(move || PdfDocument::from_bytes(data_vec))
                .await
                .map_err(|e| {
                    error!("PDF parsing task panicked: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "PDF parsing failed".to_string(),
                    )
                })?
                .map_err(|e| {
                    error!("Failed to parse PDF: {}", e);
                    (StatusCode::BAD_REQUEST, format!("Invalid PDF: {e}"))
                })?;

            let page_count = doc.page_count();
            let session_id = state.create_session(doc, filename.clone()).await.map_err(|e| {
                error!("Failed to create session: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;

            info!(
                "Created session {} for {} ({} pages)",
                session_id, filename, page_count
            );

            // POST-Redirect-GET: redirect to view page
            // HX-Redirect tells HTMX to do a full page navigation
            let redirect_url = format!("/view/{session_id}/1");
            return Response::builder()
                .status(StatusCode::OK)
                .header("HX-Redirect", redirect_url)
                .body(Body::empty())
                .or_internal_error();
        }
    }

    Err((StatusCode::BAD_REQUEST, "No file uploaded".to_string()))
}

// =============================================================================
// API Routes - HTML Fragments
// =============================================================================

/// Query params for page image.
#[derive(SerdeDeserialize, Default)]
pub struct PageImageQuery {
    #[serde(default)]
    translated: Option<String>,
}

/// Query params for page view (allows page input to use HTMX directly).
#[derive(SerdeDeserialize, Default)]
pub struct PageViewQuery {
    /// 1-based page number from the input field
    #[serde(default)]
    pub page: Option<usize>,
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
            let image_data = tokio::task::spawn_blocking(move || {
                let doc = PdfDocument::from_bytes(pdf_bytes)?;
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

            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ETAG, etag)
                .header(header::CACHE_CONTROL, "private, max-age=3600, must-revalidate")
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

/// Build Link header value for prefetching adjacent page images.
fn prefetch_links(session_id: &str, page: usize, page_count: usize) -> Option<String> {
    let mut links = Vec::new();
    if page > 0 {
        links.push(format!("</api/page/{}/{}>; rel=prefetch; as=image", session_id, page - 1));
    }
    if page + 1 < page_count {
        links.push(format!("</api/page/{}/{}>; rel=prefetch; as=image", session_id, page + 1));
    }
    if links.is_empty() {
        None
    } else {
        Some(links.join(", "))
    }
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

    let (is_translated, has_any_translations, page_count, version) = session
        .with_session(|s| {
            (
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.document.page_count(),
                s.page_store.version(page),
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
        version,
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

    let (is_translated, has_any_translations, page_count, version) = session
        .with_session(|s| {
            (
                s.page_store.has_page(page),
                !s.page_store.is_empty(),
                s.document.page_count(),
                s.page_store.version(page),
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
        version,
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

/// Query params for translation.
#[derive(SerdeDeserialize, Default)]
pub struct TranslateForm {
    /// Force re-translation, bypassing cache (Re-translate button)
    #[serde(default)]
    force: Option<String>,
    /// Page number from input field (1-based, may differ from URL if user typed a new page)
    #[serde(default)]
    page: Option<usize>,
}

/// Translate a single page - returns translated panel HTML.
///
/// HTMX: Replaces `#translated-content`, includes OOB toast and download button.
/// Re-translate button sends `force=1` in POST body to bypass cache.
/// If form.page differs from URL page, navigates to that page first (handles race condition
/// when user types a page number and clicks translate before the page navigation completes).
pub async fn translate_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
    Form(form): Form<TranslateForm>,
) -> RouteResult<TranslateResultTemplate> {
    let force_retranslate = form.force.is_some();

    // Use form.page if provided and different from URL (user typed a new page number)
    // Form page is 1-based (from input), URL page is also 1-based now, convert to 0-based
    let page = form
        .page
        .map_or_else(|| url_page.saturating_sub(1), |p| p.saturating_sub(1));
    let page_changed = form.page.is_some_and(|p| p.saturating_sub(1) != url_page.saturating_sub(1));

    debug!(
        "translate_page: url_page={}, form_page={:?}, effective_page={}, page_changed={}, force={}",
        url_page, form.page, page, page_changed, force_retranslate
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

    // Update current page in session if navigating
    if page_changed {
        session_ref
            .with_session_mut(|s| s.current_page = page)
            .await;
    }

    let translator = state.create_translator(&settings).map_err(|e| {
        error!("Failed to create translator: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    match translator.translate_page_force(&doc, page, force_retranslate).await {
        Ok(result) => {
            // Get path inside lock (fast)
            let path = session_ref
                .with_session(|s| s.page_store.page_path(page))
                .await
                .or_not_found("Session not found")?;

            // Write to disk outside lock (async)
            if let Err(e) = tokio::fs::write(&path, &result.pdf_bytes).await {
                error!("Failed to store translated page: {}", e);
            }

            // Mark stored and get version inside lock (fast)
            let version = session_ref
                .with_session_mut(|s| {
                    s.page_store.mark_stored(page);
                    s.page_store.version(page)
                })
                .await
                .unwrap_or(1);

            Ok(TranslateResultTemplate::success(
                session_id,
                page,
                page_count,
                page_changed,
                result.from_cache,
                version,
            ))
        }
        Err(e) => {
            let error_msg = e.to_string();
            error!("Translation failed for page {}: {}", page, error_msg);
            Ok(TranslateResultTemplate::error(
                session_id,
                page,
                page_count,
                page_changed,
                error_msg,
            ))
        }
    }
}

/// Start translate-all job - returns progress HTML with polling trigger.
///
/// HTMX: Replaces `#progress-area`, starts polling via `hx-trigger="every 500ms"`.
pub async fn start_translate_all(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<ProgressTemplate> {
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

    // Create job tracker
    let job = Arc::new(TranslateJob::new());
    let job_clone = Arc::clone(&job);

    // Store job reference in session
    session_ref
        .with_session_mut(|s| {
            s.translate_job = Some(Arc::clone(&job));
        })
        .await;

    // Get state clone for background task
    let state_clone = Arc::clone(&state);
    let session_id_clone = session_id.clone();

    // Spawn background translation task
    tokio::spawn(async move {
        let translator = match state_clone.create_translator(&settings) {
            Ok(t) => t,
            Err(e) => {
                job_clone.set_error(e.to_string()).await;
                job_clone.mark_done();
                return;
            }
        };

        for page in 0..page_count {
            match translator.translate_page(&doc, page).await {
                Ok(result) => {
                    if let Some(session_ref) = state_clone.get_session(&session_id_clone).await {
                        // Get path inside lock (fast)
                        let path = session_ref
                            .with_session(|s| s.page_store.page_path(page))
                            .await;

                        if let Some(path) = path {
                            // Write to disk outside lock (async)
                            if let Err(e) = tokio::fs::write(&path, &result.pdf_bytes).await {
                                error!("Failed to store translated page {}: {}", page, e);
                            } else {
                                // Mark stored inside lock (fast)
                                session_ref
                                    .with_session_mut(|s| s.page_store.mark_stored(page))
                                    .await;
                            }
                        }
                    }
                    job_clone.increment();
                }
                Err(e) => {
                    error!("Failed to translate page {}: {}", page, e);
                    job_clone
                        .set_error(format!("Failed at page {}: {}", page + 1, e))
                        .await;
                    job_clone.mark_done();
                    return;
                }
            }
        }

        job_clone.mark_done();
    });

    // Get current page and version for button restoration
    let (current_page, version) = session_ref
        .with_session(|s| (s.current_page, s.page_store.version(s.current_page)))
        .await
        .unwrap_or((0, 0));

    Ok(ProgressTemplate::new(
        session_id,
        0,
        page_count,
        format!("Translating page 1 of {page_count}..."),
        false,
        current_page,
        false,
        version,
    ))
}

/// Get translate-all status - returns progress HTML (polled by client).
///
/// HTMX: Replaces `#progress-area`. When done, stops polling and re-enables buttons.
pub async fn get_translate_all_status(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<ProgressTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (job_opt, page_count, current_page, version) = session_ref
        .with_session(|s| {
            (
                s.translate_job.clone(),
                s.document.page_count(),
                s.current_page,
                s.page_store.version(s.current_page),
            )
        })
        .await
        .or_not_found("Session not found")?;

    let job = job_opt.or_not_found("No active job")?;

    let current = job.current.load(Ordering::SeqCst);
    let done = job.done.load(Ordering::SeqCst);
    let error = job.get_error().await;
    let has_error = error.is_some();

    let message = error.unwrap_or_else(|| {
        if done {
            format!("Completed {current} of {page_count} pages")
        } else {
            format!("Translating page {} of {page_count}...", current + 1)
        }
    });

    Ok(ProgressTemplate::new(
        session_id,
        current,
        page_count,
        message,
        done,
        current_page,
        has_error,
        version,
    ))
}

/// SSE stream for translate-all progress updates.
///
/// Server pushes updates only when progress changes, eliminating polling overhead.
/// HTMX SSE extension connects to this endpoint and swaps HTML fragments.
#[allow(tail_expr_drop_order)] // Drop order change in async_stream macro is harmless here
pub async fn translate_all_stream(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let (job_opt, page_count, current_page) = session_ref
        .with_session(|s| {
            (
                s.translate_job.clone(),
                s.document.page_count(),
                s.current_page,
            )
        })
        .await
        .or_not_found("Session not found")?;

    let job = job_opt.or_not_found("No active job")?;
    let session_id_clone = session_id.clone();
    let state_clone = Arc::clone(&state);

    let stream = async_stream::stream! {
        let mut last_current = 0usize;

        loop {
            let current = job.current.load(Ordering::SeqCst);
            let done = job.done.load(Ordering::SeqCst);
            let error_future = job.get_error();
            let error = error_future.await;
            let has_error = error.is_some();

            // Only send update if progress changed or job is done
            if current != last_current || done {
                last_current = current;

                // Get version for the current page
                let session_future = state_clone.get_session(&session_id_clone);
                let session_opt = session_future.await;
                let version = if let Some(sess) = session_opt {
                    let version_future = sess.with_session(|s| s.page_store.version(current_page));
                    version_future.await.unwrap_or(0)
                } else {
                    0
                };

                let message = error.clone().unwrap_or_else(|| {
                    if done {
                        format!("Completed {current} of {page_count} pages")
                    } else {
                        format!("Translating page {} of {page_count}...", current + 1)
                    }
                });

                let template = ProgressTemplate::new(
                    session_id_clone.clone(),
                    current,
                    page_count,
                    message,
                    done,
                    current_page,
                    has_error,
                    version,
                );

                // Render template to HTML
                let render_result = template.render();
                if let Ok(html) = render_result {
                    yield Ok(Event::default().event("progress").data(html));
                }

                if done {
                    break;
                }
            }

            // Check for updates every 100ms (but only send when changed)
            let sleep_future = tokio::time::sleep(Duration::from_millis(100));
            sleep_future.await;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Settings update from form data.
#[derive(SerdeDeserialize)]
pub struct SettingsForm {
    pub source_lang: Option<String>,
    pub target_lang: Option<String>,
    pub text_color: Option<String>,
}

/// Update session settings - returns cleared panel HTML fragment.
///
/// HTMX: Replaces `#translated-content`, includes OOB swaps for flag/swatch indicators.
/// This keeps all UI state server-controlled (hypermedia-style).
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Form(update): Form<SettingsForm>,
) -> RouteResult<SettingsClearedTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Update settings and get the new values for OOB response
    let (source_lang, target_lang, text_color) = session_ref
        .with_session_mut(|s| {
            if let Some(ref source) = update.source_lang {
                s.settings.source_lang = Lang::new(source.clone());
            }
            if let Some(ref target) = update.target_lang {
                s.settings.target_lang = Lang::new(target.clone());
            }
            if let Some(ref color) = update.text_color
                && let Some(c) = TextColor::from_name(color)
            {
                s.settings.text_color = c;
            }
            // Clear translated pages when settings change
            s.page_store.clear();

            // Return current settings for OOB updates
            (
                s.settings.source_lang.as_str().to_string(),
                s.settings.target_lang.as_str().to_string(),
                update.text_color.clone().unwrap_or_else(|| "blue".to_string()),
            )
        })
        .await
        .or_not_found("Session not found")?;

    Ok(SettingsClearedTemplate::new(&source_lang, &target_lang, &text_color))
}

// =============================================================================
// API Routes - Binary Responses
// =============================================================================

/// Download translated PDF as combined document.
pub async fn download_pdf(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<Response> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Get paths and filename inside lock (fast)
    let (paths, filename) = session_ref
        .with_session(|s| {
            (s.page_store.all_page_paths(), s.original_filename.clone())
        })
        .await
        .or_not_found("Session not found")?;

    if paths.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No pages translated yet".to_string(),
        ));
    }

    // Load all pages outside lock (async)
    let mut pages = Vec::with_capacity(paths.len());
    for path in paths {
        let data = tokio::fs::read(&path).await.or_internal_error()?;
        pages.push(data);
    }

    let combined = pdf_translator_core::pdf::overlay::combine_pdfs(&pages).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to combine PDFs: {e}"),
        )
    })?;

    let download_name = format!("translated_{filename}");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{download_name}\""),
        )
        .body(Body::from(combined))
        .or_internal_error()
}
