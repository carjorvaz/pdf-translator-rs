//! Batch translation routes - translate all pages with generation-safe progress.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{
        Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream::Stream;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::error;

use crate::helpers::{OptionExt, ResultExt, RouteResult};
use crate::page_store::{PageStore, StagedPage};
use crate::state::{AppState, PageClaim, TranslateJob};
use crate::templates::ProgressTemplate;

enum BatchPage {
    Stored,
    Wait,
    Claimed(PageClaim, StagedPage),
    Stale,
}

pub async fn start_translate_all(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<Response> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Check and install the job in one session-lock transition so two POSTs
    // cannot both start paid work.
    let start = session_ref
        .with_session_mut(|session| {
            if session.active_job() {
                return None;
            }
            let job = Arc::new(TranslateJob::new(session.settings_generation));
            session.translate_job = Some(Arc::clone(&job));
            Some((
                job,
                session.settings.clone(),
                session.document.clone(),
                session.document.page_count(),
            ))
        })
        .await
        .or_not_found("Session not found")?;
    let Some((job, settings, document, page_count)) = start else {
        return Err((
            StatusCode::CONFLICT,
            "A batch translation is already active".to_string(),
        ));
    };

    let state_clone = Arc::clone(&state);
    let session_id_clone = session_id.clone();
    let job_clone = Arc::clone(&job);
    tokio::spawn(async move {
        let translator = match state_clone.create_translator(&settings) {
            Ok(translator) => translator,
            Err(error) => {
                if let Some(session) = state_clone.get_session(&session_id_clone).await {
                    session
                        .with_session_mut(|current| {
                            if current.job_is_current(&job_clone) {
                                job_clone.set_error(error.to_string());
                            }
                        })
                        .await;
                }
                return;
            }
        };

        for page in 0..page_count {
            let acquired = loop {
                let Some(session) = state_clone.get_session(&session_id_clone).await else {
                    job_clone.cancel();
                    return;
                };
                let next = session
                    .with_session_mut(|current| {
                        if !current.job_is_current(&job_clone) || !job_clone.is_active() {
                            return BatchPage::Stale;
                        }
                        if current.page_store.has_page(page) {
                            return BatchPage::Stored;
                        }
                        match current.claim_page(page) {
                            Some(claim) => {
                                BatchPage::Claimed(claim, current.page_store.staging_path(page))
                            }
                            None => BatchPage::Wait,
                        }
                    })
                    .await
                    .unwrap_or(BatchPage::Stale);
                match next {
                    BatchPage::Claimed(claim, staged) => break Some((claim, staged)),
                    BatchPage::Stored => {
                        session
                            .with_session_mut(|current| {
                                if current.job_is_current(&job_clone) && job_clone.is_active() {
                                    job_clone.increment();
                                }
                            })
                            .await;
                        break None;
                    }
                    BatchPage::Wait => tokio::time::sleep(Duration::from_millis(25)).await,
                    BatchPage::Stale => return,
                }
            };
            let Some((claim, mut staged)) = acquired else {
                continue;
            };

            let translated = match translator.translate_page(&document, page).await {
                Ok(result) => result,
                Err(error) => {
                    if let Some(session) = state_clone.get_session(&session_id_clone).await {
                        session
                            .with_session_mut(|current| {
                                current.release_claim(&claim);
                                if current.job_is_current(&job_clone) {
                                    job_clone
                                        .set_error(format!("Failed at page {}: {error}", page + 1));
                                }
                            })
                            .await;
                    }
                    return;
                }
            };

            let reservation = match state_clone.reserve_output(translated.pdf_bytes.len()) {
                Ok(reservation) => reservation,
                Err(error) => {
                    if let Some(session) = state_clone.get_session(&session_id_clone).await {
                        session
                            .with_session_mut(|current| {
                                current.release_claim(&claim);
                                if current.job_is_current(&job_clone) {
                                    job_clone.set_error(error.to_string());
                                }
                            })
                            .await;
                    }
                    return;
                }
            };
            staged.reserve(reservation);

            if let Err(write_error) = PageStore::write_staged(&staged, &translated.pdf_bytes).await
            {
                if let Some(session) = state_clone.get_session(&session_id_clone).await {
                    session
                        .with_session_mut(|current| {
                            current.release_claim(&claim);
                            if current.job_is_current(&job_clone) {
                                job_clone.set_error(format!(
                                    "Failed to store page {}: {write_error}",
                                    page + 1
                                ));
                            }
                        })
                        .await;
                }
                return;
            }

            let published = if let Some(session) = state_clone.get_session(&session_id_clone).await
            {
                session
                    .commit_claimed_page(&claim, staged, |current| {
                        current.job_is_current(&job_clone) && job_clone.is_active()
                    })
                    .await
            } else {
                Ok(None)
            };

            match published {
                Ok(Some(_)) => {
                    let progressed =
                        if let Some(session) = state_clone.get_session(&session_id_clone).await {
                            session
                                .with_session_mut(|current| {
                                    if current.job_is_current(&job_clone) && job_clone.is_active() {
                                        job_clone.increment();
                                        true
                                    } else {
                                        false
                                    }
                                })
                                .await
                                .unwrap_or(false)
                        } else {
                            false
                        };
                    if !progressed {
                        return;
                    }
                }
                Err(error) => {
                    error!("Failed to publish translated page {page}: {error}");
                    job_clone.set_error(error.to_string());
                    return;
                }
                Ok(None) => {
                    return;
                }
            }
        }

        if let Some(session) = state_clone.get_session(&session_id_clone).await {
            session
                .with_session_mut(|current| {
                    if current.job_is_current(&job_clone) && job_clone.is_active() {
                        job_clone.mark_succeeded();
                    }
                })
                .await;
        }
    });

    let template = ProgressTemplate::new(
        session_id,
        0,
        page_count,
        format!("Translating page 1 of {page_count}..."),
        false,
        false,
        true,
    );
    let html = template.render().or_internal_error()?;
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .or_internal_error()
}

#[allow(tail_expr_drop_order)]
pub async fn translate_all_stream(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;
    let (job, page_count) = session_ref
        .with_session(|session| (session.translate_job.clone(), session.document.page_count()))
        .await
        .or_not_found("Session not found")?;
    let job = job.or_not_found("No active job")?;
    let session_id_clone = session_id.clone();

    let stream = async_stream::stream! {
        let mut last_current = usize::MAX;
        loop {
            let current = job.current.load(Ordering::SeqCst);
            let done = job.is_done();
            let error = job.get_error();
            let has_error = error.is_some();
            if current != last_current || done {
                last_current = current;
                let message = error.unwrap_or_else(|| {
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
                    has_error,
                    false,
                );
                if let Ok(html) = template.render() {
                    let event = if done { "complete" } else { "progress" };
                    yield Ok(Event::default().event(event).data(html));
                }
                if done {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
