//! Download routes - PDF download handling.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::Response,
};
use std::sync::Arc;

use crate::helpers::{OptionExt, ResultExt, RouteResult};
use crate::state::AppState;

const PAGE_READ_ERROR: &str = "Failed to read translated page";
const PDF_COMBINE_ERROR: &str = "Failed to combine translated PDF";

fn public_internal_error(message: &'static str) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, message.to_string())
}

fn sanitize_filename(filename: &str) -> String {
    let sanitized: String = filename
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\') {
                '_'
            } else {
                character
            }
        })
        .collect();

    if sanitized.is_empty() {
        "document.pdf".to_string()
    } else {
        sanitized
    }
}

fn ascii_filename_fallback(filename: &str) -> String {
    filename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn encode_rfc5987(filename: &str) -> String {
    let mut encoded = String::with_capacity(filename.len());
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
        {
            encoded.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn content_disposition(filename: &str) -> String {
    let sanitized = sanitize_filename(filename);
    let download_name = format!("translated_{sanitized}");
    let fallback = ascii_filename_fallback(&download_name);
    let encoded = encode_rfc5987(&download_name);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// Download translated PDF as combined document.
pub async fn download_pdf(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<Response> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Snapshot immutable, versioned pages and the display name while holding the
    // session lock. The Arc handles keep every backing file alive through all
    // reads and PDF processing after releasing the lock.
    let (page_snapshots, filename) = session_ref
        .with_session(|session| {
            (
                session.page_store.all_page_snapshots(),
                session.original_filename.clone(),
            )
        })
        .await
        .or_not_found("Session not found")?;

    if page_snapshots.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No pages translated yet".to_string(),
        ));
    }

    let mut pages = Vec::with_capacity(page_snapshots.len());
    for snapshot in &page_snapshots {
        let data = tokio::fs::read(snapshot.path())
            .await
            .map_err(|_| public_internal_error(PAGE_READ_ERROR))?;
        pages.push(data);
    }

    let combined = tokio::task::spawn_blocking(move || {
        let result = pdf_translator_core::pdf::overlay::combine_pdfs(&pages).map_err(|_| ());
        drop(page_snapshots);
        result
    })
    .await
    .map_err(|_| public_internal_error(PDF_COMBINE_ERROR))?
    .map_err(|()| public_internal_error(PDF_COMBINE_ERROR))?;

    let content_disposition = content_disposition(&filename);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(header::CONTENT_DISPOSITION, content_disposition)
        .body(Body::from(combined))
        .or_internal_error()
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn content_disposition_has_ascii_fallback_and_utf8_filename() {
        assert_eq!(
            content_disposition("résumé 2026.pdf"),
            "attachment; filename=\"translated_r_sum__2026.pdf\"; \
             filename*=UTF-8''translated_r%C3%A9sum%C3%A9%202026.pdf"
        );
    }

    #[test]
    fn content_disposition_encodes_header_delimiters_and_replaces_controls() {
        assert_eq!(
            content_disposition("evil\";\r\nfilename=owned/文.pdf"),
            "attachment; filename=\"translated_evil____filename_owned__.pdf\"; \
             filename*=UTF-8''translated_evil%22%3B__filename%3Downed_%E6%96%87.pdf"
        );
    }
}
