//! Batch translation routes - translate all pages with progress tracking.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{sse::{Event, KeepAlive, Sse}, Response},
};
use futures::stream::Stream;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

use crate::helpers::{OptionExt, ResultExt, RouteResult};
use crate::state::{AppState, TranslateJob};
use crate::templates::ProgressTemplate;

/// Start translate-all job - returns progress HTML with SSE connection.
///
/// Returns 202 Accepted (async operation started, not completed).
/// HTMX: Replaces `#progress-area`, connects to SSE stream for updates.
pub async fn start_translate_all(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<Response> {
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

    // Get current page for button restoration
    let current_page = session_ref
        .with_session(|s| s.current_page)
        .await
        .unwrap_or(0);

    let template = ProgressTemplate::new(
        session_id,
        0,
        page_count,
        format!("Translating page 1 of {page_count}..."),
        false,
        current_page,
        false,
    );

    let html = template.render().or_internal_error()?;

    // 202 Accepted: async operation started but not completed
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .or_internal_error()
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
