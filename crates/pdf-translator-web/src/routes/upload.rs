//! Upload route - PDF file upload handling.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use axum_extra::extract::Multipart;
use axum_extra::extract::multipart::MultipartError;
use pdf_translator_core::PdfDocument;
use std::sync::Arc;
use tracing::{error, info};

use crate::UPLOAD_BODY_LIMIT;
use crate::helpers::{ResultExt, RouteResult};
use crate::state::{AppState, CreateSessionError};

fn public_multipart_error(error: &MultipartError) -> (StatusCode, String) {
    let status = error.status();
    error!("Failed to read multipart upload: {error}");
    let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
        "Upload exceeds the 64 MiB limit"
    } else {
        "Invalid multipart upload"
    };
    (status, message.to_string())
}

/// Upload a PDF file - redirects to view page (POST-Redirect-GET pattern).
///
/// Supports both HTMX requests (HX-Redirect header) and standard form submissions
/// (HTTP 303 See Other redirect) for graceful degradation without JavaScript.
pub async fn upload_pdf(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> RouteResult<Response> {
    loop {
        let Some(field) = multipart
            .next_field()
            .await
            .map_err(|error| public_multipart_error(&error))?
        else {
            break;
        };
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or("document.pdf").to_string();

            let data = field
                .bytes()
                .await
                .map_err(|error| public_multipart_error(&error))?;
            if data.len() > UPLOAD_BODY_LIMIT {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Upload exceeds the 64 MiB limit".to_string(),
                ));
            }

            // PdfDocument retains a Vec, so make the single required copy before moving parsing
            // off the async runtime.
            let data = data.to_vec();
            let doc = tokio::task::spawn_blocking(move || PdfDocument::from_bytes(data))
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
                    (StatusCode::BAD_REQUEST, "Invalid PDF document".to_string())
                })?;

            let page_count = doc.page_count();
            let session_id =
                state
                    .create_session(doc, filename.clone())
                    .await
                    .map_err(|error| {
                        error!("Failed to create session: {error}");
                        match error {
                            CreateSessionError::SessionLimit => (
                                StatusCode::TOO_MANY_REQUESTS,
                                "Too many active sessions".to_string(),
                            ),
                            CreateSessionError::StorageLimit => (
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "PDF storage limit exceeded".to_string(),
                            ),
                            CreateSessionError::PageStore(_) => (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to create session".to_string(),
                            ),
                        }
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
            }

            // Standard HTTP redirect for non-JS clients (303 See Other for POST-Redirect-GET)
            return Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(header::LOCATION, redirect_url)
                .body(Body::empty())
                .or_internal_error();
        }
    }

    Err((StatusCode::BAD_REQUEST, "No file uploaded".to_string()))
}
