//! Translation routes - single page translation and prefetching.

use askama::Template;
use axum::{
    body::Body,
    extract::{Form, Path, State},
    http::{StatusCode, header},
    response::Response,
};
use std::sync::Arc;
use tracing::{debug, error};

use super::TranslateForm;
use crate::helpers::{OptionExt, ResultExt, RouteResult, validate_page};
use crate::page_store::PageStore;
use crate::state::{AppState, PageClaim};
use crate::templates::TranslateResultTemplate;

pub async fn translate_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, url_page)): Path<(String, usize)>,
    Form(form): Form<TranslateForm>,
) -> RouteResult<Response> {
    if url_page == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Page numbers are 1-based".to_string(),
        ));
    }
    let force_retranslate = form.force.is_some();
    let page = url_page - 1;
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;
    let page_count = session_ref
        .with_session(|session| session.document.page_count())
        .await
        .or_not_found("Session not found")?;
    validate_page(page, page_count)?;

    let (cached_version, claimed) = session_ref
        .with_session_mut(|session| {
            if !force_retranslate && let Some(stored) = session.page_store.page_snapshot(page) {
                return Ok((Some(stored.version()), None));
            }
            let claim = session.claim_page(page).ok_or(())?;
            Ok((
                None,
                Some((
                    claim,
                    session.settings.clone(),
                    session.document.clone(),
                    session.page_store.staging_path(page),
                )),
            ))
        })
        .await
        .or_not_found("Session not found")?
        .map_err(|()| {
            (
                StatusCode::CONFLICT,
                "This page is already being translated".to_string(),
            )
        })?;

    if let Some(version) = cached_version {
        return render_result(&TranslateResultTemplate::success(
            session_id, page, version, true,
        ));
    }
    let Some((claim, settings, document, mut staged)) = claimed else {
        unreachable!("a translation claim accompanies every cache miss");
    };

    let translator = match state.create_translator(&settings) {
        Ok(translator) => translator,
        Err(error) => {
            release_claim(&session_ref, &claim).await;
            return render_error_result(&session_ref, session_id, page, error.to_string()).await;
        }
    };

    let result = match translator
        .translate_page_force(&document, page, force_retranslate)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            release_claim(&session_ref, &claim).await;
            error!("Translation failed for page {page}: {error}");
            return render_error_result(&session_ref, session_id, page, error.to_string()).await;
        }
    };

    let reservation = match state.reserve_output(result.pdf_bytes.len()) {
        Ok(reservation) => reservation,
        Err(error) => {
            release_claim(&session_ref, &claim).await;
            return render_error_result(&session_ref, session_id, page, error.to_string()).await;
        }
    };
    staged.reserve(reservation);

    if let Err(error) = PageStore::write_staged(&staged, &result.pdf_bytes).await {
        release_claim(&session_ref, &claim).await;
        error!("Failed to durably stage translated page {page}: {error}");
        return render_error_result(
            &session_ref,
            session_id,
            page,
            format!("Failed to store translated page: {error}"),
        )
        .await;
    }

    let publication = session_ref
        .commit_claimed_page(&claim, staged, |_| true)
        .await;

    match publication {
        Ok(Some(version)) => {
            let is_current = session_ref
                .with_session(|session| {
                    session.settings_generation == claim.generation()
                        && session.page_store.version(page) == version
                })
                .await
                .unwrap_or(false);
            if !is_current {
                return Err((
                    StatusCode::CONFLICT,
                    "Translation was superseded by updated settings".to_string(),
                ));
            }
            for next_page in [page + 1, page + 2] {
                if next_page < page_count {
                    let state = Arc::clone(&state);
                    let session_id = session_id.clone();
                    tokio::spawn(async move {
                        prefetch_page_internal(&state, &session_id, next_page).await;
                    });
                }
            }
            render_result(&TranslateResultTemplate::success(
                session_id,
                page,
                version,
                result.from_cache,
            ))
        }
        Err(error) => render_error_result(&session_ref, session_id, page, error.to_string()).await,
        Ok(None) => Err((
            StatusCode::CONFLICT,
            "Translation was superseded by updated settings".to_string(),
        )),
    }
}

async fn render_error_result(
    session_ref: &crate::state::SessionRef<'_>,
    session_id: String,
    page: usize,
    message: String,
) -> RouteResult<Response> {
    let (is_translated, version) = session_ref
        .with_session(|session| {
            session
                .page_store
                .page_snapshot(page)
                .map_or((false, 0), |stored| (true, stored.version()))
        })
        .await
        .unwrap_or((false, 0));
    render_result(&TranslateResultTemplate::error(
        session_id,
        page,
        is_translated,
        version,
        message,
    ))
}

fn render_result(template: &TranslateResultTemplate) -> RouteResult<Response> {
    let html = template.render().or_internal_error()?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .or_internal_error()
}

async fn release_claim(session_ref: &crate::state::SessionRef<'_>, claim: &PageClaim) {
    session_ref
        .with_session_mut(|session| {
            session.release_claim(claim);
        })
        .await;
}

async fn prefetch_page_internal(state: &Arc<AppState>, session_id: &str, page: usize) {
    let Some(session_ref) = state.get_session(session_id).await else {
        return;
    };
    let data = session_ref
        .with_session_mut(|session| {
            if page >= session.document.page_count() || session.page_store.has_page(page) {
                return None;
            }
            let claim = session.claim_page(page)?;
            Some((
                claim,
                session.settings.clone(),
                session.document.clone(),
                session.page_store.staging_path(page),
            ))
        })
        .await
        .flatten();
    let Some((claim, settings, document, mut staged)) = data else {
        return;
    };

    let Ok(translator) = state.create_translator(&settings) else {
        release_claim(&session_ref, &claim).await;
        return;
    };
    let Ok(result) = translator.translate_page_prefetch(&document, page).await else {
        release_claim(&session_ref, &claim).await;
        return;
    };
    let Ok(reservation) = state.reserve_output(result.pdf_bytes.len()) else {
        release_claim(&session_ref, &claim).await;
        return;
    };
    staged.reserve(reservation);
    if PageStore::write_staged(&staged, &result.pdf_bytes)
        .await
        .is_err()
    {
        release_claim(&session_ref, &claim).await;
        return;
    }

    let published = session_ref
        .commit_claimed_page(&claim, staged, |_| true)
        .await;
    if matches!(published, Ok(Some(_))) {
        debug!("Prefetch completed for page {page}");
    }
}

pub async fn prefetch_page(
    State(state): State<Arc<AppState>>,
    Path((session_id, page)): Path<(String, usize)>,
) -> StatusCode {
    tokio::spawn(async move {
        prefetch_page_internal(&state, &session_id, page).await;
    });
    StatusCode::NO_CONTENT
}
