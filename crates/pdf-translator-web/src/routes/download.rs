//! Download routes - PDF download handling.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
};
use std::sync::Arc;

use crate::helpers::{OptionExt, ResultExt, RouteResult};
use crate::state::AppState;

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
