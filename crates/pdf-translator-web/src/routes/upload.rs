//! Upload route - PDF file upload handling.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use axum_extra::extract::Multipart;
use pdf_translator_core::PdfDocument;
use std::sync::Arc;
use tracing::{error, info};

use crate::helpers::{ResultExt, RouteResult};
use crate::state::AppState;

/// Upload a PDF file - redirects to view page (POST-Redirect-GET pattern).
///
/// Supports both HTMX requests (HX-Redirect header) and standard form submissions
/// (HTTP 303 See Other redirect) for graceful degradation without JavaScript.
pub async fn upload_pdf(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
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
            let session_id = state
                .create_session(doc, filename.clone())
                .await
                .map_err(|e| {
                    error!("Failed to create session: {}", e);
                    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                })?;

            info!(
                "Created session {} for {} ({} pages)",
                session_id, filename, page_count
            );

            // POST-Redirect-GET pattern
            let redirect_url = format!("/view/{session_id}/1");

            // Check if this is an HTMX request
            let is_htmx = headers.get("HX-Request").is_some();

            if is_htmx {
                // HX-Redirect tells HTMX to do a full page navigation
                return Response::builder()
                    .status(StatusCode::OK)
                    .header("HX-Redirect", redirect_url)
                    .body(Body::empty())
                    .or_internal_error();
            } else {
                // Standard HTTP redirect for non-JS clients (303 See Other for POST-Redirect-GET)
                return Response::builder()
                    .status(StatusCode::SEE_OTHER)
                    .header(header::LOCATION, redirect_url)
                    .body(Body::empty())
                    .or_internal_error();
            }
        }
    }

    Err((StatusCode::BAD_REQUEST, "No file uploaded".to_string()))
}
